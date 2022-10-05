use std::collections::BTreeMap;
use std::sync::Arc;

use crate::database::Wallet;
use crate::signer::Signer;
use crate::state::{AppState};
use crate::walletdata::{TransactionStatus, AnnCoinID};
use async_trait::async_trait;
use base32::Alphabet;
use futures::Future;
use nanorpc::{nanorpc_derive};
use themelio_nodeprot::ValClientSnapshot;
use std::fmt::Debug;
use themelio_structs::{BlockHeight, CoinData, CoinID, Denom, Transaction, TxHash, TxKind, NetID, CoinValue, Address, CoinDataHeight};
use themelio_structs::{Header, PoolKey, PoolState};
use tmelcrypt::{Ed25519SK, HashVal, Hashable};



use melnet::{self, MelnetError};
use serde::*;

use thiserror::Error;



#[derive(Debug, Serialize, Deserialize)]
pub struct PrepareTxArgs {
    #[serde(default)]
    kind: Option<TxKind>,
    inputs: Vec<CoinID>,
    outputs: Vec<CoinData>,
    #[serde(default, with = "stdcode::hexvec")]
    covenants: Vec<Vec<u8>>,
    data: Option<String>,
    #[serde(default)]
    nobalance: Vec<Denom>,
    #[serde(default)]
    fee_ballast: usize,
    signing_key: Option<String>,
}


#[derive(Serialize, Deserialize)]
pub struct PoolInfo {
    result: u128,
    price_impact: f64,
    poolkey: String,
}
#[derive(Serialize, Deserialize)]
pub struct TxBalance(bool, TxKind, BTreeMap<String, i128>);


#[derive(Error, Debug, Deserialize, Serialize)]
pub enum RequestErrors {
    #[error("Wallet could not be found")]
    WalletNotFound,
    #[error("Bad request")]
    BadRequest(String),
    #[error("Invalid Pool Key {0}")]
    PoolKeyError(PoolKey),
    #[error("Invalid Password")]
    InvalidPassword,
    #[error("Invalid Signature")]
    InvalidSignature,
    #[error(transparent)]
    DatabaseError(#[from] crate::database::DatabaseError),
    #[error("Http error {0}")]
    HttpStatusError(http_types::StatusCode),
    #[error("Failed to unlock wallet {0}")]
    FailedUnlock(String),
    #[error("Cannot find transaction {0}")]
    TransactionNotFound(TxHash),
    #[error("Cannot submit faucet transaction on this network")]
    InvalidFaucetTransaction,
    #[error("Lost transaction {0}, no longer pending but not confirmed; probably gave up")]
    LostTransaction(TxHash),
    #[error("Failed to create wallet: {0}")]
    WalletCreationError(String),
}


impl From<MelnetError> for RequestErrors {
    fn from(_: MelnetError) -> Self {
        RequestErrors::HttpStatusError(http_types::StatusCode::BadGateway)
    }
}

impl<T> From<Option<T>> for RequestErrors {
    fn from(_: Option<T>) -> Self {
        RequestErrors::HttpStatusError(http_types::StatusCode::NotFound)
    }
}
#[nanorpc_derive]
#[async_trait]
pub trait  MelwalletdProtocol: Send + Sync {
    async fn summarize_wallet(&self, wallet_name: String) -> Result<WalletSummary, RequestErrors>;
    async fn get_summary(&self) -> Result<Header, RequestErrors>;
    async fn get_pool(&self, pool_key: PoolKey) -> Result<PoolState, RequestErrors>;
    async fn get_pool_info(
        &self,
        to: Denom,
        from: Denom,
        value: u128,
    ) -> Result<PoolInfo, RequestErrors>;
    async fn create_wallet(
        &self,
        wallet_name: String,
        password: Option<String>,
        secret: Option<String>,
    ) -> Result<(), RequestErrors>;
    async fn dump_coins(
        &self,
        wallet_name: String,
    ) -> Result<Vec<(CoinID, CoinData)>, RequestErrors>;
    async fn dump_transactions(
        &self,
        wallet_name: String,
    ) -> Result<Vec<(TxHash, Option<BlockHeight>)>, RequestErrors>;
    async fn lock_wallet(&self, wallet_name: String);
    async fn unlock_wallet(
        &self,
        wallet_name: String,
        password: Option<String>,
    ) -> Result<(), RequestErrors>;
    async fn export_sk_from_wallet(
        &self,
        wallet_name: String,
        password: Option<String>,
    ) -> Result<String, RequestErrors>;
    async fn prepare_tx(
        &self,
        wallet_name: String,
        request: PrepareTxArgs,
    ) -> Result<Transaction, RequestErrors>;
    async fn send_tx(&self, wallet_name: String, tx: Transaction) -> Result<TxHash, RequestErrors>;
    async fn get_tx_balance(
        &self,
        wallet_name: String,
        txhash: HashVal,
    ) -> Result<TxBalance, RequestErrors>;
    async fn get_tx(
        &self,
        wallet_name: String,
        txhash: HashVal,
    ) -> Result<TransactionStatus, RequestErrors>;
    async fn send_faucet(&self, wallet_name: String) -> Result<TxHash, RequestErrors>;
}


#[derive(Clone)]
pub struct MelwalletdRpcImpl {
    pub state: Arc<AppState>,
}
#[async_trait]
impl MelwalletdProtocol for MelwalletdRpcImpl {
    async fn summarize_wallet(&self, wallet_name: String) -> Result<WalletSummary, RequestErrors> {
        let state = self.state.clone();
        let wallet_list = state.list_wallets().await;
        wallet_list
            .get(&wallet_name)
            .cloned()
            .ok_or(RequestErrors::WalletNotFound)
    }
    async fn get_summary(&self) -> Result<Header, RequestErrors> {
        let state = self.state.clone();
        let client = state.client.clone();
        let snap = client.snapshot().await?;
        Ok(snap.current_header())
    }
    async fn get_pool(&self, pool_key: PoolKey) -> Result<PoolState, RequestErrors> {
        let state = self.state.clone();
        let client = state.client.clone();

        println!("You get a pool key: {}", pool_key);
        let pool_key = pool_key
            .to_canonical()
            .ok_or_else(|| RequestErrors::PoolKeyError(pool_key))?;
        client
            .snapshot()
            .await?
            .get_pool(pool_key)
            .await?
            .ok_or_else(|| RequestErrors::BadRequest("pool not found".to_owned()))
    }
    async fn get_pool_info(
        &self,
        to: Denom,
        from: Denom,
        value: u128,
    ) -> Result<PoolInfo, RequestErrors> {
        let state = self.state.clone();
        let client = state.client.clone();
        if from == to {
            return Err(RequestErrors::BadRequest(
                "cannot swap between identical denoms".to_owned(),
            ));
        }
        let pool_key = PoolKey::new(from, to);
        let pool_state = client
            .snapshot()
            .await?
            .get_pool(pool_key)
            .await?
            .ok_or_else(|| RequestErrors::BadRequest("pool not found".to_owned()))?;

        let left_to_right = pool_key.left == from;

        let r = if left_to_right {
            let old_price = pool_state.lefts as f64 / pool_state.rights as f64;
            let mut new_pool_state = pool_state;
            let (_, new) = new_pool_state.swap_many(value, 0);
            let new_price = new_pool_state.lefts as f64 / new_pool_state.rights as f64;
            PoolInfo {
                result: new,
                price_impact: (new_price / old_price - 1.0),
                poolkey: hex::encode(pool_key.to_bytes()),
            }
        } else {
            let old_price = pool_state.rights as f64 / pool_state.lefts as f64;
            let mut new_pool_state = pool_state;
            let (new, _) = new_pool_state.swap_many(0, value);
            let new_price = new_pool_state.rights as f64 / new_pool_state.lefts as f64;
            PoolInfo {
                result: new,
                price_impact: (new_price / old_price - 1.0),
                poolkey: hex::encode(pool_key.to_bytes()),
            }
        };
        Ok(r)
    }
    async fn create_wallet(
        &self,
        wallet_name: String,
        password: Option<String>,
        secret: Option<String>,
    ) -> Result<(), RequestErrors> {
        let state = self.state.clone();
        let sk = if let Some(secret) = secret {
            // We must reconstruct the secret key using the ed25519-dalek library
            let secret = base32::decode(Alphabet::Crockford, &secret).ok_or(
                RequestErrors::BadRequest("Failed to decode secret key".to_owned()),
            )?;
            let secret = ed25519_dalek::SecretKey::from_bytes(&secret)
                .map_err(|_| RequestErrors::BadRequest("failed to create secret key".to_owned()))?;
            let public: ed25519_dalek::PublicKey = (&secret).into();
            let mut vv = [0u8; 64];
            vv[0..32].copy_from_slice(&secret.to_bytes());
            vv[32..].copy_from_slice(&public.to_bytes());
            Ed25519SK(vv)
        } else {
            tmelcrypt::ed25519_keygen().1
        };
        match state.create_wallet(&wallet_name, sk, password).await {
            Ok(_) => Ok(()),
            Err(_) => Err(RequestErrors::WalletCreationError(wallet_name)), // bikeshed this more
        }
    }
    async fn dump_coins(
        &self,
        wallet_name: String,
    ) -> Result<Vec<(CoinID, CoinData)>, RequestErrors> {
        let state = self.state.clone();
        let wallet = state
            .get_wallet(&wallet_name)
            .await
            .ok_or(RequestErrors::WalletNotFound)?;
        let coins = wallet.get_coin_mapping(true, false).await;
        let coin_vec = &coins.into_iter().collect::<Vec<_>>();
        Ok(coin_vec.to_owned())
    }
    async fn dump_transactions(
        &self,
        wallet_name: String,
    ) -> Result<Vec<(TxHash, Option<BlockHeight>)>, RequestErrors> {
        let state = self.state.clone();
        let wallet = state
            .get_wallet(&wallet_name)
            .await
            .ok_or(RequestErrors::WalletNotFound)?;
        let transactions = wallet.get_transaction_history().await;
        Ok(transactions)
    }
    async fn lock_wallet(&self, wallet_name: String) {
        let state = self.state.clone();
        state.lock(&wallet_name);
    }
    async fn unlock_wallet(
        &self,
        wallet_name: String,
        password: Option<String>,
    ) -> Result<(), RequestErrors> {
        let state = self.state.clone();
        state
            .unlock(&wallet_name, password)
            .ok_or(RequestErrors::InvalidPassword)?;
        Ok(())
    }
    async fn export_sk_from_wallet(
        &self,
        wallet_name: String,
        password: Option<String>,
    ) -> Result<String, RequestErrors> {
        let state = self.state.clone();
        let secret = state
        .get_secret_key(&wallet_name, password)
        .ok_or(RequestErrors::InvalidPassword)?;
    let encoded: String = base32::encode(Alphabet::Crockford, &secret.0[..32]).into();
    Ok(encoded)
    }
    async fn prepare_tx(
        &self,
        wallet_name: String,
        request: PrepareTxArgs,
    ) -> Result<Transaction, RequestErrors> {
        let state = self.state.clone();
        let signing_key: Arc<dyn Signer> = if let Some(signing_key) = request.signing_key.as_ref() {
            Arc::new(
                signing_key
                    .parse::<Ed25519SK>()
                    .map_err(|_| RequestErrors::InvalidSignature)?,
            )
        } else {
            state
                .get_signer(&wallet_name)
                .ok_or(RequestErrors::FailedUnlock(wallet_name.clone()))?
        };
        let wallet = state
            .get_wallet(&wallet_name)
            .await
            .ok_or(RequestErrors::WalletNotFound)?;
    
        // calculate fees
        let client = state.client.clone();
        let snapshot = client.snapshot().await?;
        let fee_multiplier = snapshot.current_header().fee_multiplier;
        let kind = request.kind;
        let data = match request.data.as_ref() {
            Some(v) => Some(hex::decode(v).map_err(|_| RequestErrors::BadRequest("".to_owned()))?),
            None => None,
        };
        let prepared_tx = wallet
            .prepare(
                request.inputs.clone(),
                request.outputs.clone(),
                fee_multiplier,
                |mut tx: Transaction| {
                    if let Some(kind) = kind {
                        tx.kind = kind
                    }
                    if let Some(data) = data.clone() {
                        tx.data = data
                    }
                    tx.covenants.extend_from_slice(&request.covenants);
                    for i in 0..tx.inputs.len() {
                        tx = signing_key.sign_tx(tx, i)?;
                    }
                    Ok(tx)
                },
                request.nobalance.clone(),
                request.fee_ballast,
                state.client.snapshot().await?,
            )
            .await
            .map_err(|_| RequestErrors::BadRequest("".to_owned()))?;
    
        Ok(prepared_tx)
    }
    async fn send_tx(&self, wallet_name: String, tx: Transaction) -> Result<TxHash, RequestErrors> {
        let state = self.state.clone();
        let wallet = state
        .get_wallet(&wallet_name)
        .await
        .ok_or(RequestErrors::BadRequest("".to_owned()))?;
    let snapshot = state.client.snapshot().await?;

    // we send it off ourselves
    snapshot.get_raw().send_tx(tx.clone()).await?;
    // we mark the TX as sent in this thread.
    wallet
        .commit_sent(
            tx.clone(),
            snapshot.current_header().height + BlockHeight(10),
        )
        .await
        .map_err(|_| RequestErrors::BadRequest("".to_owned()))?;
    log::info!("sent transaction with hash {}", tx.hash_nosigs());
    let r = &tx.hash_nosigs();
    Ok(r.to_owned())
    }
    async fn get_tx_balance(
        &self,
        wallet_name: String,
        txhash: HashVal,
    ) -> Result<TxBalance, RequestErrors> {
        let state = self.state.clone();
        let wallet = state
        .get_wallet(&wallet_name)
        .await
        .ok_or(RequestErrors::WalletNotFound)?;
    let raw = wallet
        .get_transaction(txhash.into(), async { Ok(state.client.snapshot().await?) })
        .await?
        .ok_or(RequestErrors::TransactionNotFound(txhash.into()))?;

    // Is this self-originated? We check the covenants
    let self_originated = raw.covenants.iter().any(|c| c.hash() == wallet.address().0);
    // Total balance out
    let mut balance: BTreeMap<String, i128> = BTreeMap::new();
    // Add all outputs to balance

    if self_originated {
        *balance
            .entry(hex::encode(Denom::Mel.to_bytes()))
            .or_default() -= raw.fee.0 as i128;
    }
    for (idx, output) in raw.outputs.iter().enumerate() {
        let coinid = raw.output_coinid(idx as u8);
        let denom_key = hex::encode(output.denom.to_bytes());
        // first we *deduct* any balance if this self-originated
        if self_originated {
            *balance.entry(denom_key).or_default() -= output.value.0 as i128;
        }
        // then, if we find this value in our coins, we add it back. this turns out to take care of swap tx well
        if let Some(ours) = wallet.get_one_coin(coinid).await {
            let denom_key = hex::encode(ours.denom.to_bytes());
            if ours.covhash == wallet.address() {
                *balance.entry(denom_key).or_default() += ours.value.0 as i128;
            }
        }
    }
    let r = TxBalance(self_originated, raw.kind, balance);

    Ok(r)
    }
    async fn get_tx(
        &self,
        wallet_name: String,
        txhash: HashVal,
    ) -> Result<TransactionStatus, RequestErrors> {
        let state = self.state.clone();
        let wallet = state
        .get_wallet(&wallet_name)
        .await
        .ok_or(RequestErrors::WalletNotFound)?;

    let raw = wallet
        .get_cached_transaction(txhash.into())
        .await
        .ok_or(RequestErrors::TransactionNotFound(txhash.into()))?;
    let mut confirmed_height = None;
    for idx in 0..raw.outputs.len() {
        if let Some(cdh) = wallet
            .get_coin_confirmation(raw.output_coinid(idx as u8))
            .await
        {
            confirmed_height = Some(cdh.height);
        }
    }
    let outputs = raw
        .outputs
        .iter()
        .enumerate()
        .map(|(i, cd)| {
            let coin_id = raw.output_coinid(i as u8).to_string();
            let is_change = cd.covhash == wallet.address();
            let coin_data = cd.clone();
            AnnCoinID {
                coin_data,
                is_change,
                coin_id,
            }
        })
        .collect();

    if confirmed_height.is_none() {
        // Must be pending
        if !wallet.is_pending(txhash.into()).await {
            return Err(RequestErrors::LostTransaction(txhash.into()))
        }
    }
    Ok(TransactionStatus {
        raw,
        confirmed_height,
        outputs,
    })
    }
    async fn send_faucet(&self, wallet_name: String) -> Result<TxHash, RequestErrors> {
        let state = self.state.clone();
        let network = state.network;
    let wallet = state
        .get_wallet(&wallet_name)
        .await
        .ok_or(RequestErrors::WalletNotFound)?;

    // TODO: protect other networks where faucet transaction applicability is unknown
    if network == NetID::Mainnet {
        return Err(RequestErrors::InvalidFaucetTransaction);
    }
    let tx = Transaction {
        kind: TxKind::Faucet,
        inputs: vec![],
        outputs: vec![CoinData {
            covhash: wallet.address(),
            value: CoinValue::from_millions(1001u64),
            denom: Denom::Mel,
            additional_data: vec![],
        }],
        data: (0..32).map(|_| fastrand::u8(0..=255)).collect(),
        fee: CoinValue::from_millions(1001u64),
        covenants: vec![],
        sigs: vec![],
    };
    // we mark the TX as sent in this thread
    let txhash = tx.hash_nosigs();
    wallet
        .commit_sent(tx, BlockHeight(10000000000))
        .await
        .map_err(|_| RequestErrors::BadRequest("Failed to submit faucet transaction".to_owned()))?;
    Ok(txhash)
    }
}

// impl MelwalletdRpcImpl {
    // pub fn legacy_handler(&self, endpoint: Endpoint) -> impl Fn(&'_ Request<Arc<AppState>>) -> BoxFuture<'_,Result<Body, http_types::Error>>{
    //     move |req: &'_ Request<Arc<AppState>>| {
    //         match endpoint{
    //             Endpoint::Summary => todo!(),
    //             Endpoint::Pool => todo!(),
    //             Endpoint::PoolInfo => todo!(),
    //             Endpoint::ListWallets => todo!(),
    //             Endpoint::WalletSummary => {
    //                 let maybe_wallet_name = req.param("name");
    //                 async move {
    //                     let wallet_name = maybe_wallet_name?;
    //                     let wallet_summary = self.summarize_wallet(wallet_name).await?;
    //                     Body::from_json(&wallet_summary)
    //                 }.boxed()
    //             },
    //             Endpoint::CreateWallet => todo!(),
    //             Endpoint::LockWallet => todo!(),
    //             Endpoint::UnlockWallet => todo!(),
    //             Endpoint::ExportSK => todo!(),
    //             Endpoint::DumpCoins => todo!(),
    //             Endpoint::PrepareTx => todo!(),
    //             Endpoint::SendTx => todo!(),
    //             Endpoint::SendFaucet => todo!(),
    //             Endpoint::DumpTransactions => todo!(),
    //             Endpoint::GetTx => todo!(),
    //             Endpoint::GetTxBalance => todo!(),
    //         }
    //     };
    //     todo!();

        

    // }
// }