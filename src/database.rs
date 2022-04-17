use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    path::Path,
    time::Instant,
};

use anyhow::Context;
use binary_search::Direction;
use rusqlite::{params, OptionalExtension};
use stdcode::StdcodeSerializeExt;
use themelio_nodeprot::ValClientSnapshot;
use themelio_stf::melvm::{covenant_weight_from_bytes, Covenant};
use themelio_structs::{
    Address, BlockHeight, CoinData, CoinDataHeight, CoinID, CoinValue, Denom, Transaction, TxHash,
    TxKind,
};

use crate::walletdata::WalletData;

use self::pool::ConnPool;

mod pool;

/// A database that holds wallets.
#[derive(Clone)]
pub struct Database {
    pool: ConnPool,
}

impl Database {
    /// Create a new database
    pub async fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let pool = ConnPool::open(path)?;
        // then create the tables
        let conn = pool.get_conn().await;
        // *all* known coins, spent and unspent and "virtual" and whatever
        conn.execute(
            "create table if not exists coins (coinid primary key, covhash, value, denom, additional_data)",
            [],
        )?;
        conn.execute(
            "create index if not exists coins_index on coins(covhash)",
            [],
        )?;
        // all confirmed coins
        conn.execute(
            "create table if not exists coin_confirmations (coinid primary key, height not null)",
            [],
        )?;
        // all pending coins
        conn.execute(
            "create table if not exists pending_coins (coinid primary key, txhash not null)",
            [],
        )?;
        // transactions to the coins that they spend
        conn.execute(
            "create table if not exists spends (coinid primary key, txhash not null)",
            [],
        )?;
        // pending spends with expiration block height
        conn.execute(
            "create table if not exists pending (txhash primary key, expires not null)",
            [],
        )?;
        // a *cache* of all known transactions
        conn.execute(
            "create table if not exists transactions (txhash primary key, txblob not null)",
            [],
        )?;
        // wallets by name
        conn.execute(
            "create table if not exists wallet_names (name primary key, covhash not null, covenant not null)",
            [],
        )?;
        Ok(Database { pool })
    }

    /// List wallet names.
    pub async fn list_wallets(&self) -> Vec<String> {
        let conn = self.pool.get_conn().await;
        let mut rows = conn
            .prepare_cached("select name from wallet_names")
            .unwrap();
        let rows = rows.query_map(params![], |row| row.get(0)).unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    }

    /// Gets a wallet by name.
    pub async fn get_wallet(&self, name: &str) -> Option<Wallet> {
        let conn = self.pool.get_conn().await;
        let (covhash_string, covenant): (String, Vec<u8>) = conn
            .query_row(
                "select covhash, covenant from wallet_names where name = $1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .expect("db failed")?;
        let covhash: Address = covhash_string.parse().expect("malformed covhash in db");
        Some(Wallet {
            name: name.to_string(),
            covhash,
            covenant,
            pool: self.pool.clone(),
        })
    }

    /// Creates a wallet.
    pub async fn create_wallet(&self, name: &str, covenant: Covenant) -> anyhow::Result<()> {
        let covhash = covenant.hash();
        let conn = self.pool.get_conn().await;
        conn.execute(
            "insert into wallet_names values ($1, $2, $3)",
            params![name, covhash.to_string(), covenant.0],
        )?;
        Ok(())
    }

    /// Restore a wallet dump.
    pub async fn restore_wallet_dump(&self, name: &str, dump: WalletData) {
        let mut conn = self.pool.get_conn().await;
        let txn = conn.transaction().unwrap();
        for (cid, cdh) in dump.unspent_coins.into_iter() {
            txn.execute(
                "insert into coins values ($1, $2, $3, $4, $5) on conflict do nothing",
                params![
                    &cid.to_string(),
                    &cdh.coin_data.covhash.to_string(),
                    cdh.coin_data.value.0.to_string(),
                    &cdh.coin_data.denom.to_bytes(),
                    &cdh.coin_data.additional_data.clone(),
                ],
            )
            .expect("db failed");
            txn.execute(
                "insert into coin_confirmations values ($1, $2) on conflict do nothing",
                params![cid.to_string(), cdh.height.0],
            )
            .expect("fail");
        }
        txn.execute(
            "insert into wallet_names values ($1, $2, $3) on conflict (name) do update set covhash = excluded.covhash",
            params![name, dump.my_covenant.hash().to_string(), dump.my_covenant.0],
        )
        .expect("db failed");
        txn.commit().unwrap();
    }

    /// Retransmit pending transactions
    pub async fn retransmit_pending(&self, snapshot: ValClientSnapshot) -> anyhow::Result<()> {
        let mut conn = self.pool.get_conn().await;
        let txn = conn.transaction()?;
        let mut stmt =
            txn.prepare_cached("select txblob from pending natural join transactions")?;
        let mut rows = stmt.query(params![]).unwrap();
        while let Ok(Some(row)) = rows.next() {
            let blob: Vec<u8> = row.get(0)?;
            let txn: Transaction = stdcode::deserialize(&blob)?;
            log::debug!("retransmit {}", txn.hash_nosigs());
            let snapshot = snapshot.clone();
            smolscale::spawn(async move {
                if let Err(err) = snapshot.get_raw().send_tx(txn).await {
                    log::warn!("error retransmitting: {:?}", err);
                }
            })
            .detach();
        }
        drop(rows);
        drop(stmt);
        Ok(())
    }
}

/// A wallet within a database
pub struct Wallet {
    name: String,
    covhash: Address,
    covenant: Vec<u8>,
    pool: ConnPool,
}

impl Wallet {
    /// Covenant hash
    pub fn address(&self) -> Address {
        self.covhash
    }

    /// Obtains a transaction, whether cached or not. Must provide a snapshot to retrieve non-cached transactions.
    pub async fn get_transaction(
        &self,
        txhash: TxHash,
        fut_snapshot: impl Future<Output = anyhow::Result<ValClientSnapshot>>,
    ) -> anyhow::Result<Option<Transaction>> {
        // if cached, get cached
        if let Some(tx) = self.get_cached_transaction(txhash).await {
            return Ok(Some(tx));
        }
        // otherwise, let's try to find a coinid that came from this txhash. (otherwise this txhash isn't even relevant and we don't care)
        let (_, cdh) = {
            let mut ctr = 0;
            loop {
                if ctr > 10 {
                    return Ok(None);
                }
                let coinid = CoinID::new(txhash, ctr);
                if let Some(confirm) = self.get_coin_confirmation(coinid).await {
                    break (coinid, confirm);
                }
                ctr += 1;
            }
        };
        // now great, we've found a relevant coin and where that coin was created. this gives us enough info to find the actual transaction.
        let txn = if let Some(txn) = fut_snapshot
            .await?
            .get_older(cdh.height)
            .await?
            .get_transaction(txhash)
            .await?
        {
            txn
        } else {
            return Ok(None);
        };
        // now we can actually put it back into the cache so that next time we don't need to do all this.
        let conn = self.pool.get_conn().await;
        conn.execute(
            "insert into transactions values ($1, $2) on conflict do nothing",
            params![txhash.to_string(), txn.stdcode()],
        )?;
        Ok(Some(txn))
    }

    /// Obtains a cached transaction.
    pub async fn get_cached_transaction(&self, txhash: TxHash) -> Option<Transaction> {
        let conn = self.pool.get_conn().await;
        let blob: Vec<u8> = conn
            .query_row(
                "select txblob from transactions where txhash = $1",
                params![txhash.to_string()],
                |row| row.get(0),
            )
            .optional()
            .unwrap()?;
        let txn: Transaction = stdcode::deserialize(&blob).unwrap();
        Some(txn)
    }

    /// Check whether a particular txhash is pending.
    pub async fn is_pending(&self, txhash: TxHash) -> bool {
        let conn = self.pool.get_conn().await;
        conn.query_row(
            "select txhash from pending where txhash = $1",
            params![txhash.to_string()],
            |_| Ok(()),
        )
        .optional()
        .unwrap()
        .is_some()
    }

    /// Gets the balance by denomination.
    pub async fn get_balances(&self) -> BTreeMap<Denom, CoinValue> {
        let mut toret = BTreeMap::new();
        log::trace!("calling get_coin_mapping from get_balances");
        for (_, data) in self.get_coin_mapping(false, false).await {
            *toret.entry(data.denom).or_default() += data.value;
        }
        toret
    }

    /// Obtains transaction history.
    pub async fn get_transaction_history(&self) -> Vec<(TxHash, Option<BlockHeight>)> {
        // We infer the transaction history through our coin confirmations
        let conn = self.pool.get_conn().await;
        let mut stmt = conn
            .prepare_cached(
                r"select coins.coinid, height from 
        coins left join coin_confirmations
        on coins.coinid = coin_confirmations.coinid
        where covhash = $1",
            )
            .unwrap();
        let mut rows = stmt.query(params![self.covhash.to_string()]).unwrap();
        let mut toret = HashMap::new();
        while let Ok(Some(row)) = rows.next() {
            let coinid: String = row.get(0).unwrap();
            let coinid: CoinID = coinid.parse().unwrap();
            let height: Option<u64> = row.get(1).unwrap();
            if let Some(height) = height {
                if coinid == CoinID::proposer_reward(height.into()) {
                    continue;
                }
            }
            toret.insert(coinid.txhash, height.map(|h| h.into()));
        }
        let mut out = toret.into_iter().collect::<Vec<_>>();
        out.sort_unstable_by_key(|x| x.1);
        out
    }

    /// Gets all the coins in the wallet, filtered by confirmation and spent status.
    pub async fn get_coin_mapping(
        &self,
        confirmed: bool,
        ignore_pending: bool,
    ) -> BTreeMap<CoinID, CoinData> {
        let start = Instant::now();
        scopeguard::defer!(log::trace!("get_coin_mapping took {:?}", start.elapsed()));
        let conn = self.pool.get_conn().await;
        let stmt = match (confirmed, ignore_pending) {
            (true, true) => {
                r"select coinid, value, denom, additional_data from coins where 
                covhash = $1
                and exists (select height from coin_confirmations where coin_confirmations.coinid = coins.coinid)
                and not exists (select txhash from spends where spends.coinid = coins.coinid 
                    and not exists (select txhash from pending where spends.txhash = pending.txhash))"
            }
            (true, false) => {
                r"select coinid,  value, denom, additional_data from coins where 
                covhash = $1
                and exists (select height from coin_confirmations where coin_confirmations.coinid = coins.coinid)
                and not exists (select txhash from spends where spends.coinid = coins.coinid)"
            }
            (false, true) => {
                r"select coinid,  value, denom, additional_data from coins where 
                covhash = $1
                and (exists (select coinid from coin_confirmations where coin_confirmations.coinid = coins.coinid)
                    or exists (select coinid from pending_coins where pending_coins.coinid = coins.coinid))
                and not exists (select txhash from spends where spends.coinid = coins.coinid 
                    and not exists (select txhash from pending where spends.txhash = pending.txhash))"
            }
            (false, false) => {
                r"select coinid,  value, denom, additional_data from coins where 
                covhash = $1
                and (exists (select coinid from coin_confirmations where coin_confirmations.coinid = coins.coinid)
                     or exists (select coinid from pending_coins where pending_coins.coinid = coins.coinid))
                and not exists (select txhash from spends where spends.coinid = coins.coinid)"
            }
        };
        let mut stmt = conn.prepare_cached(stmt).unwrap();
        let mut rows = stmt.query(params![self.covhash.to_string()]).unwrap();
        let mut toret = BTreeMap::new();
        while let Ok(Some(row)) = rows.next() {
            let coinid: String = row.get(0).unwrap();
            let value: String = row.get(1).unwrap();
            let denom: Vec<u8> = row.get(2).unwrap();
            let additional_data: Vec<u8> = row.get(3).unwrap();
            let value: CoinValue = CoinValue(value.parse().unwrap());
            let denom: Denom = Denom::from_bytes(&denom).unwrap();
            let cdata = CoinData {
                covhash: self.covhash,
                value,
                denom,
                additional_data,
            };
            let coinid: CoinID = coinid.parse().unwrap();
            toret.insert(coinid, cdata);
        }
        toret
    }

    /// Prepares transactions
    pub async fn prepare(
        &self,
        inputs: Vec<CoinID>,
        outputs: Vec<CoinData>,
        fee_multiplier: u128,
        sign: impl Fn(Transaction) -> anyhow::Result<Transaction>,
        nobalance: Vec<Denom>,
    ) -> anyhow::Result<Transaction> {
        let mut nobalance = nobalance;
        nobalance.push(Denom::NewCoin);
        let nobalance = nobalance;
        let mut mandatory_inputs = BTreeMap::new();
        // first we add the "mandatory" inputs
        for input in inputs {
            let coindata = self
                .get_coin_confirmation(input)
                .await
                .context("mandatory input not found in wallet")?;
            mandatory_inputs.insert(input, coindata.clone());
        }
        log::trace!("calling get_coin_mapping from prepare");
        let unspent_coins = self.get_coin_mapping(true, false).await;
        let gen_transaction = |fee| {
            log::debug!("trying with a fee of {} MEL", fee);
            let start = Instant::now();
            // find coins that might match
            let mut txn = Transaction {
                kind: TxKind::Normal,
                inputs: vec![],
                outputs: outputs.clone(),
                fee,
                covenants: vec![self.covenant.clone()],
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

            log::trace!("before unspent coins: {:?}", start.elapsed());

            // then we add random other inputs until enough.
            // we filter out everything that is in the stake list.

            log::trace!("after shuffling unspent coins: {:?}", start.elapsed());

            for (coin, data) in unspent_coins.iter() {
                // blacklist of coins
                if mandatory_inputs.contains_key(coin)
                    || nobalance.contains(&data.denom)
                    || data.covhash != self.covhash
                {
                    // do not consider it
                    continue;
                }
                let existing_val = input_sum.get(&data.denom).cloned().unwrap_or(CoinValue(0));
                if existing_val < output_sum.get(&data.denom).cloned().unwrap_or(CoinValue(0)) {
                    txn.inputs.push(*coin);
                    input_sum.insert(data.denom, existing_val + data.value);
                }
            }

            log::trace!("after going through unspent coins: {:?}", start.elapsed());

            // create change outputs
            let change = {
                let mut change = Vec::new();
                for (cointype, sum) in output_sum.iter() {
                    let difference = input_sum
                        .get(cointype)
                        .cloned()
                        .unwrap_or(CoinValue(0))
                        .checked_sub(*sum);
                    if let Some(difference) = difference {
                        if difference.0 > 0 || *cointype == Denom::Mel {
                            // We make TWO change outputs, to maximize parallelization
                            // TODO: does this create indefinitely many UTXOs? That'd be bad
                            if difference.0 >= 2 {
                                let first_half = difference / 2;
                                let second_half = difference - first_half;
                                change.push(CoinData {
                                    covhash: self.covhash,
                                    value: first_half,
                                    denom: *cointype,
                                    additional_data: vec![],
                                });
                                change.push(CoinData {
                                    covhash: self.covhash,
                                    value: second_half,
                                    denom: *cointype,
                                    additional_data: vec![],
                                })
                            } else {
                                change.push(CoinData {
                                    covhash: self.covhash,
                                    value: difference,
                                    denom: *cointype,
                                    additional_data: vec![],
                                })
                            }
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

            log::trace!("before signing: {:?}", start.elapsed());
            log::debug!("candidate with {} inputs", txn.inputs.len());
            if txn.inputs.len() > 5000 {
                return Direction::High(Err(anyhow::anyhow!("too many inputs")));
            }

            if !txn.is_well_formed() {
                log::error!("somehow produced an obviously ill-formed TX: {:?}", txn);
                return Direction::High(Err(anyhow::anyhow!("transaction not well-formed")));
            }
            let signed_txn = sign(txn);
            log::trace!("after signing: {:?}", start.elapsed());
            match signed_txn {
                Ok(signed_txn) => {
                    if signed_txn.fee
                        <= signed_txn.base_fee(fee_multiplier, 0, covenant_weight_from_bytes) * 21
                            / 20
                    {
                        Direction::Low(Ok(signed_txn))
                    } else {
                        Direction::High(Ok(signed_txn))
                    }
                }
                Err(err) => Direction::Low(Err(err)),
            }
        };
        let max_fee: CoinValue = unspent_coins
            .values()
            .filter(|cdh| cdh.denom == Denom::Mel)
            .map(|d| d.value)
            .sum();
        let max_fee = match gen_transaction(CoinValue(0u128)) {
            Direction::Low(Ok(t)) => {
                t.base_fee(fee_multiplier, 0, covenant_weight_from_bytes) * 3 + CoinValue(100)
            }
            Direction::High(Ok(t)) => {
                t.base_fee(fee_multiplier, 0, covenant_weight_from_bytes) * 3 + CoinValue(100)
            }
            _ => max_fee,
        };
        let (_, (_, val)) = binary_search::binary_search(
            (0u128, Err(anyhow::anyhow!("nothing"))),
            (max_fee.0, Err(anyhow::anyhow!("nothing"))),
            |a| gen_transaction(CoinValue(a)),
        );
        log::debug!("prepared TX with fee {:?}", val.as_ref().map(|v| v.fee));
        val.context("preparation failed")
    }

    /// Sets transactions as sent
    pub async fn commit_sent(&self, txn: Transaction, timeout: BlockHeight) -> anyhow::Result<()> {
        let mut conn = self.pool.get_conn().await;
        let conn = conn.transaction()?;
        // ensure that every input is available
        for input in txn.inputs.iter() {
            if conn
                .query_row(
                    "select height from coin_confirmations where coinid = $1",
                    params![input.to_string()],
                    |_| Ok(()),
                )
                .optional()?
                .is_none()
            {
                anyhow::bail!("input {} no longer in wallet", input)
            }
        }
        // add the transaction to the cache
        let txhash = txn.hash_nosigs();
        conn.execute(
            "insert into transactions values ($1, $2) on conflict do nothing",
            params![txhash.to_string(), txn.stdcode()],
        )?;
        // spend everything
        for input in txn.inputs.iter() {
            conn.execute(
                "insert into spends values ($1, $2)",
                params![input.to_string(), txhash.to_string()],
            )?;
        }

        // ONLY do this if this is a NORMAL transaction. Otherwise transmutation will invalidate these coins BADLY.
        if txn.kind == TxKind::Normal {
            for (i, output) in txn.outputs.iter().enumerate() {
                let coinid = txn.output_coinid(i as u8);
                let denom = if output.denom == Denom::NewCoin {
                    Denom::Custom(txn.hash_nosigs())
                } else {
                    output.denom
                };
                conn.execute(
                    "insert into coins values ($1, $2, $3, $4, $5) on conflict do nothing",
                    params![
                        coinid.to_string(),
                        output.covhash.to_string(),
                        output.value.0.to_string(),
                        denom.to_bytes(),
                        output.additional_data.clone()
                    ],
                )?;
                conn.execute(
                    "insert into pending_coins values ($1, $2)",
                    params![coinid.to_string(), txn.hash_nosigs().to_string()],
                )?;
            }
        }
        // add to pending
        conn.execute(
            "insert into pending values ($1, $2)",
            params![txhash.to_string(), timeout.0],
        )?;
        // commit
        conn.commit()?;
        Ok(())
    }

    /// Gets any coin.
    pub async fn get_one_coin(&self, coin_id: CoinID) -> Option<CoinData> {
        let conn = self.pool.get_conn().await;
        let result: (String, String, Vec<u8>, Vec<u8>) = conn
            .query_row(
                "select covhash, value, denom, additional_data from coins where coinid = $1",
                [coin_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .unwrap()?;
        let cd = CoinData {
            covhash: result.0.parse().unwrap(),
            value: CoinValue(result.1.parse().unwrap()),
            denom: Denom::from_bytes(&result.2).unwrap(),
            additional_data: result.3,
        };
        Some(cd)
    }

    /// Gets the confirmation status of a coin.
    pub async fn get_coin_confirmation(&self, coin_id: CoinID) -> Option<CoinDataHeight> {
        let coindata = self.get_one_coin(coin_id).await?;
        let conn = self.pool.get_conn().await;
        let height: u64 = conn
            .query_row(
                "select height from coin_confirmations where coinid = $1",
                [coin_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .unwrap()?;
        Some(CoinDataHeight {
            height: height.into(),
            coin_data: coindata,
        })
    }

    /// Updates the list of coins, given a network snapshot.
    pub async fn network_sync(&self, snapshot: ValClientSnapshot) -> anyhow::Result<()> {
        // The basic idea is that we get the list of coins from the remote, then add them all to the wallet.
        // However, we also need to take care of "disappearing" coins. If we have a confirmed coin that is no longer in the latest set, it must have been spent somewhere along the way. If we don't already have the transactions that spends it in the "spends", we must find that transaction through a binary search between the block where that coin was confirmed and the current block --- otherwise we cannot mark that coin as spent.

        // First find the pending count
        let pending_count: u64 = {
            let conn = self.pool.get_conn().await;
            conn.query_row("select count(txhash) from pending", params![], |r| r.get(0))
                .unwrap()
        };

        // First step is to get the list of coins
        // TODO something more efficient
        let remote_coin_count = snapshot
            .get_coin_count(self.covhash)
            .await?
            .unwrap_or_default();
        // Then, we compare with the coins we already have
        log::trace!("calling coin_mapping from sync");
        let existing_coins = self.get_coin_mapping(true, false).await;
        if existing_coins.len() == remote_coin_count as usize
            && pending_count == 0
            && fastrand::f64() < 0.95
        // occasionally do a full sync
        {
            return Ok(());
        }
        // reconstruct the coin list
        let remote_coin_list = snapshot
            .get_raw()
            .get_some_coins(snapshot.current_header().height, self.covhash)
            .await?
            .context("get_some_coins returned nothing")?;
        if remote_coin_list.len() != remote_coin_count as usize {
            anyhow::bail!("remote coin list is bad")
        }
        let mut coin_list = BTreeMap::new();
        let mut potential_coins = vec![];
        for coinid in remote_coin_list.iter().copied() {
            if let Some(cdh) = self.get_coin_confirmation(coinid).await {
                coin_list.insert(coinid, cdh);
            } else {
                let snapshot = snapshot.clone();
                let task = smolscale::spawn(async move {
                    let start = Instant::now();
                    let res = snapshot
                        .get_coin(coinid)
                        .await?
                        .context("self-contradictory coin list");
                    res
                });
                potential_coins.push((coinid, task));
            }
        }
        for (coinid, task) in potential_coins {
            log::debug!("resolving coinid {} => {}", coinid, coin_list.len());
            let cdh = task.await?;
            coin_list.insert(coinid, cdh);
        }

        // We take care of all disappearing coins
        let mut new_spenders = HashSet::new();
        let mut skip = HashSet::new();
        for (disappeared_coin, _) in existing_coins
            .iter()
            .filter(|c| !coin_list.contains_key(c.0))
        {
            if skip.contains(disappeared_coin) {
                continue;
            }
            if let Some(cdh) = self.get_coin_confirmation(*disappeared_coin).await {
                let height = cdh.height;
                log::debug!(
                    "wallet {} lost coin {}, finding out why",
                    self.name,
                    disappeared_coin
                );
                // binary search for the first block in which this was gone
                let mut left = height;
                let mut right = snapshot.current_header().height;
                while left < right {
                    let median = (left + right) / 2;
                    log::trace!("binary search at {} ({}..{})", median, left, right);
                    if snapshot
                        .get_older(median)
                        .await?
                        .get_coin(*disappeared_coin)
                        .await?
                        .is_some()
                    {
                        left = median + BlockHeight(1);
                    } else {
                        right = median;
                    }
                }
                let spend_block = left;
                let spender_tx = snapshot
                    .get_older(spend_block)
                    .await?
                    .current_block()
                    .await?
                    .transactions
                    .into_iter()
                    .find(|tx| tx.inputs.contains(disappeared_coin))
                    .expect("bug: digged for the coin in the wrong place");
                log::debug!("found spender: {}", spender_tx.hash_nosigs());
                // we don't have to revisit other coins this same spender spends
                for input in spender_tx.inputs.iter() {
                    skip.insert(*input);
                }
                new_spenders.insert(spender_tx);
            }
        }
        // we insert the coins and spenders in one atomic  transaction
        let mut conn = self.pool.get_conn().await;
        let txn = conn.transaction()?;
        for (coin, cdh) in coin_list {
            txn.execute(
                "insert into coins values ($1, $2, $3, $4, $5) on conflict do nothing",
                params![
                    coin.to_string(),
                    cdh.coin_data.covhash.to_string(),
                    cdh.coin_data.value.0.to_string(),
                    cdh.coin_data.denom.to_bytes(),
                    cdh.coin_data.additional_data
                ],
            )
            .unwrap();
            txn.execute(
                "insert into coin_confirmations values ($1, $2) on conflict do nothing",
                params![coin.to_string(), cdh.height.0],
            )
            .unwrap();
        }
        for spender in new_spenders {
            let txhash = spender.hash_nosigs();
            for input in spender.inputs {
                txn.execute(
                    "insert into spends values ($1, $2) on conflict do nothing",
                    params![input.to_string(), txhash.to_string()],
                )?;
            }
        }

        // remove all pendings that have confirmation
        let confirmed_txhashes: HashSet<TxHash> =
            remote_coin_list.into_iter().map(|c| c.txhash).collect();
        for txhash in confirmed_txhashes {
            txn.execute(
                "delete from pending where txhash = $1",
                params![txhash.to_string()],
            )?;
        }

        // Finally, we remove all stupid pending things
        txn.execute("delete from spends where exists (select expires from pending where expires < $1 and txhash = spends.txhash)", params![snapshot.current_header().height.0])?;

        txn.execute(
            "delete from pending where expires < $1",
            params![snapshot.current_header().height.0],
        )?;

        // remove all pending coins that no longer correspond to pending
        txn.execute("delete from pending_coins where not exists (select expires from pending where pending.txhash = pending_coins.txhash)", params![])?;

        txn.commit()?;
        Ok(())
    }
}
