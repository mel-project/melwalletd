use std::collections::BTreeMap;

use anyhow::Context;
use binary_search::Direction;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use themelio_stf::{
    melvm::Covenant, CoinData, CoinDataHeight, CoinID, Denom, NetID, Transaction, TxHash, TxKind,
    MAX_COINVAL,
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
    tx_confirmed: BTreeMap<TxHash, (Transaction, u64)>,
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

    /// Spent Coins
    pub fn spent_coins(&self) -> &BTreeMap<CoinID, CoinDataHeight> {
        &self.spent_coins
    }

    /// In-progress transactions
    pub fn tx_in_progress(&self) -> &BTreeMap<TxHash, Transaction> {
        &self.tx_in_progress
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
        let gen_transaction = |fee| {
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

            let mut input_sum: BTreeMap<Denom, u128> = BTreeMap::new();
            // first we add the "mandatory" inputs
            for (coin, data) in mandatory_inputs.iter() {
                txn.inputs.push(*coin);
                let existing_val = input_sum.get(&data.coin_data.denom).cloned().unwrap_or(0);
                input_sum.insert(data.coin_data.denom, existing_val + data.coin_data.value);
            }

            // don't try to balance the nobalance stuff
            for denom in nobalance.iter() {
                output_sum.remove(denom);
                input_sum.remove(denom);
            }

            // then we add random other inputs until enough.
            for (coin, data) in self.unspent_coins.iter() {
                if mandatory_inputs.contains_key(coin) {
                    // we should not add a mandatory input back in
                    continue;
                }
                let existing_val = input_sum.get(&data.coin_data.denom).cloned().unwrap_or(0);
                if existing_val < output_sum.get(&data.coin_data.denom).cloned().unwrap_or(0) {
                    txn.inputs.push(*coin);
                    input_sum.insert(data.coin_data.denom, existing_val + data.coin_data.value);
                }
            }

            // create change outputs
            let change = {
                let mut change = Vec::new();
                for (cointype, sum) in output_sum.iter() {
                    let difference = input_sum.get(cointype).unwrap_or(&0).checked_sub(*sum);
                    if let Some(difference) = difference {
                        if difference > 0 || *cointype == Denom::Mel {
                            // We *always* make at least one change output
                            change.push(CoinData {
                                covhash: self.my_covenant.hash(),
                                value: difference,
                                denom: *cointype,
                                additional_data: vec![],
                            })
                        }
                    } else {
                        return Direction::High(None);
                    }
                }
                change
            };
            txn.outputs.extend(change.into_iter());
            assert!(txn.is_well_formed());
            let signed_txn = sign(txn);
            if let Ok(signed_txn) = signed_txn {
                if signed_txn.fee <= signed_txn.base_fee(fee_multiplier, 0) * 21 / 20 {
                    Direction::Low(Some(signed_txn))
                } else {
                    Direction::High(Some(signed_txn))
                }
            } else {
                Direction::High(None)
            }
        };
        let (_, (_, val)) =
            binary_search::binary_search((0u128, None), (MAX_COINVAL, None), gen_transaction);
        log::debug!("prepared TX with fee {:?}", val.as_ref().map(|v| v.fee));
        val.context("not enough money")
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
                anyhow::bail!("did not supply covhash")
            }
            oself.spent_coins.insert(input, coindata);
        }
        // put tx in progress
        oself.tx_in_progress.insert(txn.hash_nosigs(), txn);
        // "commit"
        *self = oself;
        Ok(())
    }

    /// Informs the state of a confirmed transaction, based on its txhash. This will move the transaction from the in-progress to confirmed.
    pub fn commit_confirmed(&mut self, txhash: TxHash, height: u64) {
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
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TransactionStatus {
    pub raw: Transaction,
    pub confirmed_height: Option<u64>,
    pub outputs: Vec<AnnCoinID>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AnnCoinID {
    pub coin_data: CoinData,
    pub is_change: bool,
    pub coin_id: String,
}
