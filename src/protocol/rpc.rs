use std::{collections::BTreeMap, sync::Arc};

use crate::state::AppState;
use anyhow::Context;
use async_trait::async_trait;
use base32::Alphabet;

use http_types::Body;
use melwalletd_prot::{
    types::{
        AnnCoinID, CreateWalletError, NeedWallet, NetworkError, PrepareTxArgs, PrepareTxError,
        SwapInfo, TransactionStatus, TxBalance, WalletAccessError, WalletSummary,
    },
    MelwalletdProtocol, MelwalletdService,
};
use nanorpc::RpcService;
use stdcode::SerializeAsString;
use themelio_structs::{
    BlockHeight, CoinData, CoinID, CoinValue, Denom, Header, NetID, PoolKey, PoolState,
    Transaction, TxHash, TxKind,
};
use tide::{Request, Server};
use tmelcrypt::{Ed25519SK, HashVal, Hashable};

#[async_trait]
impl MelwalletdProtocol for AppState {
    async fn list_wallets(&self) -> Vec<String> {
        self.list_wallets().await.keys().cloned().collect()
    }

    async fn wallet_summary(
        &self,
        wallet_name: String,
    ) -> Result<WalletSummary, WalletAccessError> {
        let wallet_list = self.list_wallets().await;
        wallet_list
            .get(&wallet_name)
            .cloned()
            .ok_or(WalletAccessError::NotFound)
    }

    async fn latest_header(&self) -> Result<Header, NetworkError> {
        let snap = self
            .client()
            .snapshot()
            .await
            .map_err(|e| NetworkError::Transient(e.to_string()))?;
        Ok(snap.current_header().into())
    }

    async fn melswap_info(
        &self,
        pool_key: SerializeAsString<PoolKey>,
    ) -> Result<Option<PoolState>, NetworkError> {
        let pool_key = pool_key
            .0
            .to_canonical()
            .ok_or_else(|| NetworkError::Fatal("invalid pool key".into()))?;

        let snapshot = self
            .client()
            .snapshot()
            .await
            .map_err(|e| NetworkError::Transient(e.to_string()))?;

        let pool = snapshot
            .get_pool(pool_key)
            .await
            .map_err(|e| NetworkError::Transient(e.to_string()))?;
        Ok(pool)
    }

    async fn simulate_swap(
        &self,
        to: SerializeAsString<Denom>,
        from: SerializeAsString<Denom>,
        value: u128,
    ) -> Result<Option<SwapInfo>, NetworkError> {
        let pool_key = PoolKey {
            left: to.0,
            right: from.0,
        };
        let pool_key = pool_key
            .to_canonical()
            .ok_or_else(|| NetworkError::Fatal("invalid pool key".into()))?;

        let pool_state = if let Some(state) = self
            .client()
            .snapshot()
            .await
            .map_err(|e| NetworkError::Transient(e.to_string()))?
            .get_pool(pool_key)
            .await
            .map_err(|e| NetworkError::Transient(e.to_string()))?
        {
            state
        } else {
            return Ok(None);
        };

        let left_to_right = pool_key.left == from.0;

        let r = if left_to_right {
            let old_price = pool_state.lefts as f64 / pool_state.rights as f64;
            let mut new_pool_state = pool_state;
            let (_, new) = new_pool_state.swap_many(value, 0);
            let new_price = new_pool_state.lefts as f64 / new_pool_state.rights as f64;
            SwapInfo {
                result: new,
                slippage: ((new_price - old_price) * 1_000_000.0) as u128,
                poolkey: hex::encode(pool_key.to_bytes()),
            }
        } else {
            let old_price = pool_state.rights as f64 / pool_state.lefts as f64;
            let mut new_pool_state = pool_state;
            let (new, _) = new_pool_state.swap_many(0, value);
            let new_price = new_pool_state.rights as f64 / new_pool_state.lefts as f64;
            SwapInfo {
                result: new,
                slippage: ((new_price - old_price) * 1_000_000.0) as u128,
                poolkey: hex::encode(pool_key.to_bytes()),
            }
        };
        Ok(Some(r))
    }

    async fn create_wallet(
        &self,
        wallet_name: String,
        password: String,
        secret: Option<String>,
    ) -> Result<(), CreateWalletError> {
        let sk = if let Some(secret) = secret {
            // We must reconstruct the secret key using the ed25519-dalek library
            let secret = base32::decode(Alphabet::Crockford, &secret).ok_or_else(|| {
                CreateWalletError::SecretKey("Failed to decode secret key".to_owned())
            })?;
            let secret = ed25519_dalek::SecretKey::from_bytes(&secret).map_err(|_| {
                CreateWalletError::SecretKey("Failed to create secret key".to_owned())
            })?;
            let public: ed25519_dalek::PublicKey = (&secret).into();
            let mut vv = [0u8; 64];
            vv[0..32].copy_from_slice(&secret.to_bytes());
            vv[32..].copy_from_slice(&public.to_bytes());
            Ed25519SK(vv)
        } else {
            tmelcrypt::ed25519_keygen().1
        };
        match self.create_wallet_inner(&wallet_name, sk, password).await {
            Ok(_) => Ok(()),
            Err(e) => Err(CreateWalletError::Other(e.to_string())),
        }
    }

    async fn dump_coins(
        &self,
        wallet_name: String,
    ) -> Result<Vec<(CoinID, CoinData)>, WalletAccessError> {
        let wallet = self
            .get_wallet(&wallet_name)
            .await
            .ok_or(WalletAccessError::NotFound)?;
        let coins = wallet.get_coin_mapping(true, false).await;
        Ok(coins.into_iter().collect())
    }

    async fn dump_transactions(
        &self,
        wallet_name: String,
    ) -> Result<Vec<(TxHash, Option<BlockHeight>)>, WalletAccessError> {
        let wallet = self
            .get_wallet(&wallet_name)
            .await
            .ok_or(WalletAccessError::NotFound)?;
        let transactions = wallet.get_transaction_history().await;
        Ok(transactions)
    }

    async fn lock_wallet(&self, wallet_name: String) -> Result<(), WalletAccessError> {
        // TODO check wallet existence. Blocked on better wallet backend logic
        self.lock(&wallet_name);
        Ok(())
    }

    async fn unlock_wallet(
        &self,
        wallet_name: String,
        password: String,
    ) -> Result<(), WalletAccessError> {
        // TODO handle the wallet not found case correctly
        self.unlock(&wallet_name, password)
            .ok_or(WalletAccessError::Locked)?;
        Ok(())
    }

    async fn export_sk(
        &self,
        wallet_name: String,
        password: String,
    ) -> Result<String, WalletAccessError> {
        let secret = self
            .get_secret_key(&wallet_name, &password)
            .map_err(|_| WalletAccessError::Locked)?
            .ok_or(WalletAccessError::NotFound)?;

        // We always return Some right now. In the future, when we have cool stuff like hardware wallets, we might return None.
        let encoded: String = base32::encode(Alphabet::Crockford, &secret.0[..32]);
        Ok(encoded)
    }

    async fn prepare_tx(
        &self,
        wallet_name: String,
        request: PrepareTxArgs,
    ) -> Result<Transaction, NeedWallet<PrepareTxError>> {
        let signing_key = self
            .get_signer(&wallet_name)
            .ok_or(NeedWallet::Wallet(WalletAccessError::NotFound))?;
        let is_locked = self
            .wallet_summary(wallet_name.clone())
            .await
            .context("can't fail by not finding the wallet since the line above would catch that")
            .unwrap()
            .locked;
        if is_locked{
            return Err(NeedWallet::Wallet(WalletAccessError::Locked))
        }
        let wallet = self
            .get_wallet(&wallet_name)
            .await
            .context("the wallet both exists and is unlocked at this point")
            .unwrap();

        // calculate fees
        let snapshot = self
            .client()
            .snapshot()
            .await
            .map_err(|e| PrepareTxError::Network(NetworkError::Transient(e.to_string())))?;
        let fee_multiplier = snapshot.current_header().fee_multiplier;

        let sign = {
            let covenants = request.covenants.clone();
            let kind = request.kind;
            let data = request.data;
            move |mut tx: Transaction| {
                tx.kind = kind;

                tx.data = data.clone();

                tx.covenants.extend_from_slice(&covenants);
                for i in 0..tx.inputs.len() {
                    tx = signing_key.sign_tx(tx, i)?;
                }
                Ok(tx)
            }
        };
        // TODO this returns the wrong error. We should have Wallet return a PrepareTxError.
        let prepared_tx = wallet
            .prepare(
                request.inputs.clone(),
                request.outputs.clone(),
                fee_multiplier,
                Arc::new(Box::new(sign)),
                request.nobalance.clone(),
                request.fee_ballast,
                self.client()
                    .snapshot()
                    .await
                    .map_err(|e| PrepareTxError::Network(NetworkError::Transient(e.to_string())))?,
            )
            .await
            .map_err(|e| PrepareTxError::Network(NetworkError::Fatal(e.to_string())))?;

        Ok(prepared_tx.into())
    }

    async fn send_tx(
        &self,
        wallet_name: String,
        tx: Transaction,
    ) -> Result<TxHash, NeedWallet<NetworkError>> {
        let tx: Transaction = tx;
        let wallet = self
            .get_wallet(&wallet_name)
            .await
            .ok_or(NeedWallet::Wallet(WalletAccessError::NotFound))?;
        let snapshot = self
            .client()
            .snapshot()
            .await
            .map_err(|e| NetworkError::Transient(e.to_string()))?;

        // we send it off ourselves
        snapshot
            .get_raw()
            .send_tx(tx.clone())
            .await
            .map_err(|e| NetworkError::Transient(e.to_string()))?
            .map_err(|e| NetworkError::Fatal(e.to_string()))?;

        // we mark the TX as sent in this thread.
        wallet
            .commit_sent(
                tx.clone(),
                snapshot.current_header().height + BlockHeight(10),
            )
            .await
            .map_err(|e| NetworkError::Fatal(e.to_string()))?;
        log::info!("sent transaction with hash {}", tx.hash_nosigs());
        Ok(tx.hash_nosigs())
    }

    async fn tx_balance(
        &self,
        wallet_name: String,
        txhash: HashVal,
    ) -> Result<Option<TxBalance>, WalletAccessError> {
        let wallet = self
            .get_wallet(&wallet_name)
            .await
            .ok_or(WalletAccessError::NotFound)?;

        // TODO the backend should expose infallible methods for these things, and do the network sync in the background. That way, network failures would just delay the time at which txx are marked confirmed, rather than causing failures.
        // The current approach is incorrect and returns a misleading error message.
        let snapshot = self
            .client()
            .snapshot()
            .await
            .map_err(|e| WalletAccessError::Other(e.to_string()))?;
        let raw = wallet
            .get_transaction(txhash.into(), snapshot)
            .await
            .map_err(|e| WalletAccessError::Other(e.to_string()))?;
        let raw = if let Some(raw) = raw {
            raw
        } else {
            return Ok(None);
        };

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

        Ok(Some(r.into()))
    }

    async fn tx_status(
        &self,
        wallet_name: String,
        txhash: HashVal,
    ) -> Result<Option<TransactionStatus>, WalletAccessError> {
        let wallet = if let Some(wallet) = self.get_wallet(&wallet_name).await {
            wallet
        } else {
            return Ok(None);
        };

        let raw = if let Some(wallet) = wallet.get_cached_transaction(txhash.into()).await {
            wallet
        } else {
            return Ok(None);
        };
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
                // we forgot about the transaction, lawl
                // TODO this should just be handled by the backend clearing these transactions out
                return Ok(None);
            }
        }
        Ok(Some(TransactionStatus {
            raw: raw.into(),
            confirmed_height,
            outputs,
        }))
    }

    async fn send_faucet(&self, wallet_name: String) -> Result<TxHash, NeedWallet<NetworkError>> {
        let network = self.get_network();
        let wallet = self
            .get_wallet(&wallet_name)
            .await
            .ok_or(NeedWallet::Wallet(WalletAccessError::NotFound))?;

        // TODO: protect other networks where faucet transaction applicability is unknown
        if network == NetID::Mainnet {
            return Err(NetworkError::Fatal("faucets don't work on mainnet".into()).into());
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
            .map_err(|e| NetworkError::Fatal(e.to_string()))?;
        Ok(txhash)
    }
}

/// Starts the RPC tide route
pub fn route_rpc(app: &mut Server<AppState>) {
    app.at("").post(move |mut r: Request<AppState>| {
        let service = r.state().clone();
        async move {
            let request_body: nanorpc::JrpcRequest = r.body_json().await?;
            let service = MelwalletdService(service);
            let rpc_res = service.respond_raw(request_body).await;
            Body::from_json(&rpc_res)
        }
    });
}
