use std::collections::BTreeMap;

use anyhow::Context;
use binary_search::Direction;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use themelio_stf::{
    melvm::Covenant, BlockHeight, CoinData, CoinDataHeight, CoinID, CoinValue, Denom, NetID,
    StakeDoc, Transaction, TxHash, TxKind, MAX_COINVAL, STAKE_EPOCH,
};

/// Cloneable in-memory data that can be persisted.
/// Does not store secrets!
#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WalletData {
    #[serde_as(as = "Vec<(_, _)>")]
    unspent_coins: BTreeMap<CoinID, CoinDataHeight>,
    #[serde_as(as = "Vec<(_, _)>")]
    spent_coins: BTreeMap<CoinID, CoinDataHeight>,
    tx_in_progress: BTreeMap<TxHash, Transaction>,
    tx_confirmed: BTreeMap<TxHash, (Transaction, BlockHeight)>,
    #[serde(default)]
    stake_list: BTreeMap<TxHash, StakeDoc>,
    my_covenant: Covenant,
    network: NetID,
}

impl WalletData {
    /// Create a new data.
    pub fn new(my_covenant: Covenant, network: NetID) -> Self {
        WalletData {
            unspent_coins: BTreeMap::new(),
            spent_coins: BTreeMap::new(),
            tx_in_progress: BTreeMap::new(),
            tx_confirmed: BTreeMap::new(),
            my_covenant,
            network,
            stake_list: BTreeMap::new(),
        }
    }

    /// Obtain a reference to network
    pub fn network(&self) -> NetID {
        self.network
    }

    /// Obtain a reference to my covenant
    pub fn my_covenant(&self) -> &Covenant {
        &self.my_covenant
    }

    /// Unspent Coins
    pub fn unspent_coins(&self) -> &BTreeMap<CoinID, CoinDataHeight> {
        &self.unspent_coins
    }

    /// Unspent Coins
    pub fn unspent_coins_mut(&mut self) -> &mut BTreeMap<CoinID, CoinDataHeight> {
        &mut self.unspent_coins
    }

    /// Spent Coins
    pub fn spent_coins(&self) -> &BTreeMap<CoinID, CoinDataHeight> {
        &self.spent_coins
    }

    /// Stake list
    pub fn stake_list(&self) -> &BTreeMap<TxHash, StakeDoc> {
        &self.stake_list
    }

    /// In-progress transactions
    pub fn tx_in_progress(&self) -> &BTreeMap<TxHash, Transaction> {
        &self.tx_in_progress
    }

    /// Forcibly reverts an in-progress transaction
    pub fn force_revert_tx(&mut self, txhash: TxHash) {
        if let Some(tx) = self.tx_in_progress.remove(&txhash) {
            // un-spend all the coins spent by this tx
            for input in tx.inputs.iter() {
                if let Some(coin) = self.spent_coins.remove(input) {
                    self.unspent_coins.insert(*input, coin);
                }
            }
            self.stake_list.remove(&txhash);
        }
    }

    /// Inserts a coin into the data, returning whether or not the coin already exists.
    pub fn insert_coin(&mut self, coin_id: CoinID, coin_data_height: CoinDataHeight) -> bool {
        self.commit_confirmed(coin_id.txhash, coin_data_height.height);
        self.spent_coins.get(&coin_id).is_none()
            && self
                .unspent_coins
                .insert(coin_id, coin_data_height)
                .is_none()
    }

    /// Convenience method to prepare a staking transaction that sends the staked syms back to oneself.
    pub fn prepare_stake(
        &self,
        stake_doc: StakeDoc,
        fee_multiplier: u128,
        sign: impl Fn(Transaction) -> anyhow::Result<Transaction>,
    ) -> anyhow::Result<Transaction> {
        let frozen_output = CoinData {
            covhash: self.my_covenant().hash(),
            value: stake_doc.syms_staked,
            denom: Denom::Sym,
            additional_data: vec![],
        };
        self.prepare(
            vec![],
            vec![frozen_output],
            fee_multiplier,
            move |mut tx| {
                tx.kind = TxKind::Stake;
                tx.data = stdcode::serialize(&stake_doc).unwrap();
                sign(tx)
            },
            vec![],
        )
    }

    /// Creates an **unsigned** transaction out of the coins in the data. Does not spend it yet.
    pub fn prepare(
        &self,
        inputs: Vec<CoinID>,
        outputs: Vec<CoinData>,
        fee_multiplier: u128,
        sign: impl Fn(Transaction) -> anyhow::Result<Transaction>,
        nobalance: Vec<Denom>,
    ) -> anyhow::Result<Transaction> {
        let mut mandatory_inputs = BTreeMap::new();
        // first we add the "mandatory" inputs
        for input in inputs {
            let coindata = self
                .unspent_coins
                .get(&input)
                .context("mandatory input not found in wallet")?;
            mandatory_inputs.insert(input, coindata.clone());
        }
        let gen_transaction = |fee: u128| {
            let fee = CoinValue(fee);
            // find coins that might match
            let mut txn = Transaction {
                kind: TxKind::Normal,
                inputs: vec![],
                outputs: outputs.clone(),
                fee,
                scripts: vec![self.my_covenant.clone()],
                data: vec![],
                sigs: vec![],
            };

            // compute output sum
            let mut output_sum = txn.total_outputs();

            let mut input_sum: BTreeMap<Denom, CoinValue> = BTreeMap::new();
            // first we add the "mandatory" inputs
            for (coin, data) in mandatory_inputs.iter() {
                txn.inputs.push(*coin);
                let existing_val = input_sum
                    .get(&data.coin_data.denom)
                    .cloned()
                    .unwrap_or(CoinValue(0));
                input_sum.insert(data.coin_data.denom, existing_val + data.coin_data.value);
            }

            // don't try to balance the nobalance stuff
            for denom in nobalance.iter() {
                output_sum.remove(denom);
                input_sum.remove(denom);
            }

            // then we add random other inputs until enough.
            // we filter out everything that is in the stake list.
            for (coin, data) in self.unspent_coins.iter() {
                // blacklist of coins
                if mandatory_inputs.contains_key(coin)
                    || nobalance.contains(&data.coin_data.denom)
                    || self.stake_list.contains_key(&coin.txhash) && coin.index == 0
                    || data.coin_data.covhash != self.my_covenant().hash()
                {
                    // do not consider it
                    continue;
                }
                let existing_val = input_sum
                    .get(&data.coin_data.denom)
                    .cloned()
                    .unwrap_or(CoinValue(0));
                if existing_val
                    < output_sum
                        .get(&data.coin_data.denom)
                        .cloned()
                        .unwrap_or(CoinValue(0))
                {
                    txn.inputs.push(*coin);
                    input_sum.insert(data.coin_data.denom, existing_val + data.coin_data.value);
                }
            }

            // create change outputs
            let change = {
                let mut change = Vec::new();
                for (cointype, sum) in output_sum.iter() {
                    let difference = input_sum
                        .get(cointype)
                        .unwrap_or(&CoinValue(0))
                        .0
                        .checked_sub(sum.0)
                        .map(CoinValue);
                    if let Some(difference) = difference {
                        if difference > CoinValue(0) || *cointype == Denom::Mel {
                            // We *always* make at least one change output
                            change.push(CoinData {
                                covhash: self.my_covenant.hash(),
                                value: difference,
                                denom: *cointype,
                                additional_data: vec![],
                            })
                        }
                    } else {
                        return Direction::High(Err(anyhow::anyhow!(
                            "not enough money for denomination {}",
                            cointype
                        )));
                    }
                }
                change
            };
            txn.outputs.extend(change.into_iter());
            if !txn.is_well_formed() {
                return Direction::High(Err(anyhow::anyhow!("transaction not well-formed")));
            }
            let signed_txn = sign(txn);
            match signed_txn {
                Ok(signed_txn) => {
                    if signed_txn.fee <= signed_txn.base_fee(fee_multiplier, 0) * 21 / 20 {
                        Direction::Low(Ok(signed_txn))
                    } else {
                        Direction::High(Ok(signed_txn))
                    }
                }
                Err(err) => Direction::Low(Err(err)),
            }
        };
        let (_, (_, val)) = binary_search::binary_search(
            (0, Err(anyhow::anyhow!("nothing"))),
            (MAX_COINVAL.0, Err(anyhow::anyhow!("nothing"))),
            gen_transaction,
        );
        log::debug!("prepared TX with fee {:?}", val.as_ref().map(|v| v.fee));
        val.context("preparation failed")
    }

    /// Informs the state of a sent transaction. This transaction must only spend coins that are in the wallet. Such a transaction can be created using [WalletData::prepare].
    pub fn commit_sent(&mut self, txn: Transaction) -> anyhow::Result<()> {
        // we clone self to guarantee error-safety
        let mut oself = self.clone();
        if !txn.is_well_formed() {
            anyhow::bail!("not well-formed")
        }
        let scripts = txn.script_as_map();
        // move coins from spent to unspent
        for input in txn.inputs.iter().cloned() {
            let coindata = oself
                .unspent_coins
                .remove(&input)
                .ok_or_else(|| anyhow::anyhow!("no such coin in data"))?;
            if scripts.get(&coindata.coin_data.covhash).is_none() {
                let scripts = scripts
                    .keys()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                anyhow::bail!(
                    "attempted to spend a coin with covhash {}, but scripts have covhashes {}",
                    coindata.coin_data.covhash,
                    scripts
                )
            }
            oself.spent_coins.insert(input, coindata);
        }
        // freeze the transaction if it is a staking transaction
        if txn.kind == TxKind::Stake {
            let sdoc: StakeDoc = stdcode::deserialize(&txn.data)
                .context("stake transaction must have a stake-doc data")?;
            oself.stake_list.insert(txn.hash_nosigs(), sdoc);
        }
        // put tx in progress
        oself.tx_in_progress.insert(txn.hash_nosigs(), txn);
        // "commit"
        *self = oself;
        Ok(())
    }

    /// Informs the state of a confirmed transaction, based on its txhash. This will move the transaction from the in-progress to confirmed.
    pub fn commit_confirmed(&mut self, txhash: TxHash, height: BlockHeight) {
        if let Some(tx) = self.tx_in_progress.remove(&txhash) {
            self.tx_confirmed.insert(txhash, (tx, height));
        }
    }

    /// Gets status of a transaction
    pub fn get_tx_status(&self, txhash: TxHash) -> Option<TransactionStatus> {
        let (confirmed_height, raw) = if let Some((tx, height)) = self.tx_confirmed.get(&txhash) {
            (Some(*height), tx.clone())
        } else if let Some(tx) = self.tx_in_progress.get(&txhash) {
            (None, tx.clone())
        } else {
            return None;
        };
        let outputs = raw
            .outputs
            .iter()
            .enumerate()
            .map(|(i, cd)| {
                let coin_id = raw.output_coinid(i as u8).to_string();
                let is_change = cd.covhash == self.my_covenant.hash();
                let coin_data = cd.clone();
                AnnCoinID {
                    coin_data,
                    is_change,
                    coin_id,
                }
            })
            .collect();
        Some(TransactionStatus {
            raw,
            confirmed_height,
            outputs,
        })
    }

    /// Filter out everything in the stake list that's too old
    pub fn retain_valid_stakes(&mut self, current_height: BlockHeight) {
        let current_epoch = current_height.0 / STAKE_EPOCH;
        self.stake_list.retain(|_, v| v.e_post_end >= current_epoch);
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TransactionStatus {
    pub raw: Transaction,
    pub confirmed_height: Option<BlockHeight>,
    pub outputs: Vec<AnnCoinID>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AnnCoinID {
    pub coin_data: CoinData,
    pub is_change: bool,
    pub coin_id: String,
}
