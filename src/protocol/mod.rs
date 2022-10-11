pub mod legacy;

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::Arc;

use melwalletd_prot::error::ProtocolError::Endo;

use melwalletd_prot::error::{
    self, to_exo, InvalidPassword, MelnetError, NeedWallet, NeverError, PoolKeyError,
    ProtocolError, StateError, TransactionError,
};
use melwalletd_prot::signer::Signer;

use async_trait::async_trait;
use base32::Alphabet;
use melwalletd_prot::request_errors::{CreateWalletError, PrepareTxError};
use melwalletd_prot::types::{
    Melwallet, MelwalletdHelpers, PoolInfo, PrepareTxArgs, TxBalance, WalletSummary,
};
use melwalletd_prot::walletdata::{AnnCoinID, TransactionStatus};
use themelio_structs::{
    BlockHeight, CoinData, CoinID, CoinValue, Denom, NetID, Transaction, TxHash, TxKind,
};
use themelio_structs::{Header, PoolKey, PoolState};
use tmelcrypt::{Ed25519SK, HashVal, Hashable};

use melwalletd_prot::protocol::MelwalletdProtocol;

#[derive(Clone)]
pub struct MelwalletdRpcImpl<State: MelwalletdHelpers> {
    pub state: Arc<State>,
}

unsafe impl<State: MelwalletdHelpers> Send for MelwalletdRpcImpl<State> {}
unsafe impl<State: MelwalletdHelpers> Sync for MelwalletdRpcImpl<State> {}

impl<State: MelwalletdHelpers + Send + Sync> MelwalletdRpcImpl<State> {
    pub fn new(state: Arc<State>) -> Self {
        MelwalletdRpcImpl {
            state,
        }
    }
}
#[async_trait]
impl<State: MelwalletdHelpers + Send + Sync> MelwalletdProtocol
    for MelwalletdRpcImpl<State>
{
    async fn summarize_wallet(
        &self,
        wallet_name: String,
    ) -> Result<WalletSummary, NeedWallet<NeverError>> {
        let state = self.state.clone();
        let wallet_list = state.list_wallets().await;
        wallet_list
            .get(&wallet_name)
            .cloned()
            .ok_or(NeedWallet::NotFound(wallet_name))
    }

    async fn get_summary(&self) -> Result<Header, error::MelnetError> {
        let state = self.state.clone();
        let client = state.client().clone();
        let snap = client.snapshot().await?;
        Ok(snap.current_header())
    }

    /// get a pool by poolkey,
    /// can fail by:
    ///     providing an invalid poolkey like MEL/MEL
    ///     inability to create snapshot
    /// returns None if pool doesn't exist
    async fn get_pool(
        &self,
        pool_key: PoolKey,
    ) -> Result<Option<PoolState>, StateError<PoolKeyError>> {
        let pool_key = pool_key
            .to_canonical()
            .ok_or(error::PoolKeyError(pool_key))?;

        let state = self.state.clone();
        let client = state.client().clone();

        let snapshot = client.snapshot().await.map_err(to_exo)?;

        let pool = snapshot.get_pool(pool_key).await.map_err(to_exo)?;
        Ok(pool)
    }

    /// simulate swapping one asset for another
    /// can fail :
    ///     bad pool key
    ///     failed snapshot
    /// None if pool doesn't exist
    async fn simulate_pool_swap(
        &self,
        to: Denom,
        from: Denom,
        value: u128,
    ) -> Result<Option<PoolInfo>, StateError<PoolKeyError>> {
        let pool_key = PoolKey {
            left: to,
            right: from,
        };
        let pool_key = pool_key
            .to_canonical()
            .ok_or(error::PoolKeyError(pool_key))?;

        let state = self.state.clone();
        let client = state.client().clone();

        let maybe_pool_state = client
            .snapshot()
            .await
            .map_err(to_exo)?
            .get_pool(pool_key)
            .await
            .map_err(to_exo)?;

        if maybe_pool_state.is_none() {
            return Ok(None);
        }

        let pool_state = maybe_pool_state.unwrap();

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
        Ok(Some(r))
    }
    /// ErrorEnum => CreateWalletError; SecretKeyError WalletCreationError
    async fn create_wallet(
        &self,
        wallet_name: String,
        password: Option<String>,
        secret: Option<String>,
    ) -> Result<(), CreateWalletError> {
        let state = self.state.clone();
        let sk = if let Some(secret) = secret {
            // We must reconstruct the secret key using the ed25519-dalek library
            let secret = base32::decode(Alphabet::Crockford, &secret)
                .ok_or_else(|| error::SecretKeyError("Failed to decode secret key".to_owned()))?;
            let secret = ed25519_dalek::SecretKey::from_bytes(&secret)
                .map_err(|_| error::SecretKeyError("Failed to create secret key".to_owned()))?;
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
            Err(e) => Err(error::WalletCreationError(e.to_string()).into()), // bikeshed this more
        }
    }

    async fn dump_coins(
        &self,
        wallet_name: String,
    ) -> Result<Vec<(CoinID, CoinData)>, NeedWallet<NeverError>> {
        let state = self.state.clone();
        let wallet = state
            .get_wallet(&wallet_name)
            .await
            .ok_or(NeedWallet::NotFound(wallet_name))?;
        let coins = wallet.get_coin_mapping(true, false).await;
        let coin_vec = &coins.into_iter().collect::<Vec<_>>();
        Ok(coin_vec.to_owned())
    }
    async fn dump_transactions(
        &self,
        wallet_name: String,
    ) -> Result<Vec<(TxHash, Option<BlockHeight>)>, NeedWallet<NeverError>> {
        let state = self.state.clone();
        let wallet = state
            .get_wallet(&wallet_name)
            .await
            .ok_or(NeedWallet::NotFound(wallet_name))?;
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
    ) -> Result<(), InvalidPassword> {
        let state = self.state.clone();
        state
            .unlock(&wallet_name, password)
            .ok_or(error::InvalidPassword)?;
        Ok(())
    }

    async fn export_sk_from_wallet(
        &self,
        wallet_name: String,
        password: Option<String>,
    ) -> Result<Option<String>, InvalidPassword> {
        let state = self.state.clone();
        let maybe_secret = state.get_secret_key(&wallet_name, password)?;

        if maybe_secret.is_none() {
            return Ok(None);
        }

        let secret = maybe_secret.unwrap();

        let encoded: String = base32::encode(Alphabet::Crockford, &secret.0[..32]);
        Ok(Some(encoded))
    }

    /// ErrorEnum => PrepareTxError; InvalidSignature FailedUnlock
    async fn prepare_tx(
        &self,
        wallet_name: String,
        request: PrepareTxArgs,
    ) -> Result<Transaction, ProtocolError<NeedWallet<PrepareTxError>, MelnetError>> {
        let state = self.state.clone();
        let signing_key: Arc<dyn Signer> = if let Some(signing_key) = request.signing_key.as_ref() {
            Arc::new(
                signing_key
                    .parse::<Ed25519SK>()
                    .map_err(|_| error::InvalidSignature)
                    .map_err(|e| Endo(NeedWallet::Other(PrepareTxError::InvalidSignature(e))))?,
            )
        } else {
            state
                .get_signer(&wallet_name)
                .ok_or(error::FailedUnlock)
                .map_err(|e| Endo(PrepareTxError::FailedUnlock(e).into()))?
        };
        let wallet = state
            .get_wallet(&wallet_name)
            .await
            .ok_or(NeedWallet::NotFound(wallet_name))?;

        // calculate fees
        let client = state.client().clone();
        let snapshot = client.snapshot().await.map_err(to_exo)?;
        let fee_multiplier = snapshot.current_header().fee_multiplier;

        let sign = {
            let covenants = request.covenants.clone();
            let kind = request.kind.clone();
            let data = match request.data.as_ref() {
                Some(v) => hex::decode(v).ok(),
                None => None,
            };
            move |mut tx: Transaction| {
                if let Some(kind) = kind {
                    tx.kind = kind
                }
                if let Some(data) = data.clone() {
                    tx.data = data
                }
                tx.covenants.extend_from_slice(&covenants);
                for i in 0..tx.inputs.len() {
                    tx = signing_key.sign_tx(tx, i)?;
                }
                Ok(tx)
            }
        };
        let prepared_tx = wallet
            .prepare(
                request.inputs.clone(),
                request.outputs.clone(),
                fee_multiplier,
                Arc::new(Box::new(sign)),
                request.nobalance.clone(),
                request.fee_ballast,
                state.client().snapshot().await.map_err(to_exo)?,
            )
            .await
            .map_err(|_| ProtocolError::BadRequest("".to_owned()))?;

        Ok(prepared_tx)
    }

    async fn send_tx(
        &self,
        wallet_name: String,
        tx: Transaction,
    ) -> Result<TxHash, StateError<NeedWallet<NeverError>>> {
        let state = self.state.clone();
        let wallet = state
            .get_wallet(&wallet_name)
            .await
            .ok_or(NeedWallet::NotFound(wallet_name))?;
        let snapshot = state.client().snapshot().await.map_err(to_exo)?;

        // we send it off ourselves
        snapshot
            .get_raw()
            .send_tx(tx.clone())
            .await
            .map_err(to_exo)?;
        // we mark the TX as sent in this thread.
        wallet
            .commit_sent(
                tx.clone(),
                snapshot.current_header().height + BlockHeight(10),
            )
            .await
            .map_err(|_| ProtocolError::BadRequest("".to_owned()))?;
        log::info!("sent transaction with hash {}", tx.hash_nosigs());
        let r = &tx.hash_nosigs();
        Ok(r.to_owned())
    }
    async fn get_tx_balance(
        &self,
        wallet_name: String,
        txhash: HashVal,
    ) -> Result<TxBalance, StateError<NeedWallet<TransactionError>>> {
        let state = self.state.clone();
        let wallet = state
            .get_wallet(&wallet_name)
            .await
            .ok_or(NeedWallet::NotFound(wallet_name))
            .map_err(Endo)?;

        let snapshot = state.client().snapshot().await.map_err(to_exo)?;
        let raw = wallet
            .get_transaction(txhash.into(), snapshot)
            .await
            .transpose()
            .ok_or_else(|| TransactionError::NotFound(txhash.into()))
            .map_err(|e| Endo(e.into()))?
            .map_err(to_exo)?;

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
    ) -> Result<TransactionStatus, StateError<NeedWallet<TransactionError>>> {
        let state = self.state.clone();
        let wallet = state
            .get_wallet(&wallet_name)
            .await
            .ok_or(NeedWallet::NotFound(wallet_name))
            .map_err(Endo)?;

        let raw = wallet
            .get_cached_transaction(txhash.into())
            .await
            .ok_or_else(|| TransactionError::NotFound(txhash.into()))
            .map_err(|e| Endo(NeedWallet::Other(e)))?;
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
                return Err(Endo(NeedWallet::Other(TransactionError::Lost(
                    txhash.into(),
                ))));
            }
        }
        Ok(TransactionStatus {
            raw,
            confirmed_height,
            outputs,
        })
    }

    async fn send_faucet(
        &self,
        wallet_name: String,
    ) -> Result<TxHash, StateError<NeedWallet<TransactionError>>> {
        let state = self.state.clone();
        let network = state.get_network();
        let wallet = state
            .get_wallet(&wallet_name)
            .await
            .ok_or(NeedWallet::NotFound(wallet_name))
            .map_err(Endo)?;

        // TODO: protect other networks where faucet transaction applicability is unknown
        if network == NetID::Mainnet {
            return Err(Endo(NeedWallet::Other(TransactionError::InvalidFaucet)));
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
            .map_err(|_| {
                ProtocolError::Exo(MelnetError(
                    "Failed to submit faucet transaction".to_owned(),
                ))
            })?;
        Ok(txhash)
    }
}

// impl MelwalletdRpcImpl {
//     pub fn legacy_handler(&self, endpoint: Endpoint) -> impl Fn(&'_ Request<Arc<AppState>>) -> BoxFuture<'_,Result<Body, http_types::Error>>{
//         move |req: &'_ Request<Arc<AppState>>| {
//             match endpoint{
//                 Endpoint::Summary => todo!(),
//                 Endpoint::Pool => todo!(),
//                 Endpoint::PoolInfo => todo!(),
//                 Endpoint::ListWallets => todo!(),
//                 Endpoint::WalletSummary => {
//                     let maybe_wallet_name = req.param("name");
//                     async move {
//                         let wallet_name = maybe_wallet_name?;
//                         let wallet_summary = self.summarize_wallet(wallet_name).await?;
//                         Body::from_json(&wallet_summary)
//                     }.boxed()
//                 },
//                 Endpoint::CreateWallet => todo!(),
//                 Endpoint::LockWallet => todo!(),
//                 Endpoint::UnlockWallet => todo!(),
//                 Endpoint::ExportSK => todo!(),
//                 Endpoint::DumpCoins => todo!(),
//                 Endpoint::PrepareTx => todo!(),
//                 Endpoint::SendTx => todo!(),
//                 Endpoint::SendFaucet => todo!(),
//                 Endpoint::DumpTransactions => todo!(),
//                 Endpoint::GetTx => todo!(),
//                 Endpoint::GetTxBalance => todo!(),
//             }
//         };
//         todo!();

//     }
// }
