use std::{collections::BTreeMap, net::SocketAddr, sync::Arc, time::Duration};

use crate::{
    database::Database,
    secrets::{EncryptedSK, PersistentSecret, SecretStore},
};

use async_trait::async_trait;
use dashmap::DashMap;
use melwalletd_prot::{
    error::InvalidPassword,
    signer::Signer,
    types::{Melwallet, MelwalletdHelpers, WalletSummary},
};
use smol_timeout::TimeoutExt;
use themelio_nodeprot::ValClient;
use themelio_stf::melvm::Covenant;
use themelio_structs::{Denom, NetID};
use tmelcrypt::Ed25519SK;

/// Encapsulates all the state and logic needed for the wallet daemon.
pub struct AppState {
    pub database: Database,
    pub network: NetID,
    pub _client: ValClient,
    pub unlocked_signers: DashMap<String, Arc<dyn Signer>>,
    pub secrets: SecretStore,
    pub _confirm_task: smol::Task<()>,
    // pub trusted_height: TrustedHeight,
}

impl AppState {
    pub fn new(
        database: Database,
        network: NetID,
        secrets: SecretStore,
        _addr: SocketAddr,
        _client: ValClient,
    ) -> Self {
        let _confirm_task = smolscale::spawn(confirm_task(database.clone(), _client.clone()));

        Self {
            database,
            network,
            _client,
            unlocked_signers: Default::default(),
            secrets,
            _confirm_task,
        }
    }
}
///themelio_bootstrap::checkpoint_height(network).unwrap()
#[async_trait]
impl MelwalletdHelpers for AppState {
    fn client(&self) -> ValClient {
        self._client.clone()
    }

    fn get_network(&self) -> NetID {
        self.network
    }
    /// Creates a new appstate, given a network server `addr`.

    /// Returns a summary of wallets.
    async fn list_wallets(&self) -> BTreeMap<String, WalletSummary> {
        let mlist = self.database.list_wallets().await;
        let mut toret = BTreeMap::new();
        for name in mlist.into_iter() {
            let wallet = self.database.get_wallet(&name).await.unwrap();
            let balance = wallet.get_balances().await;
            let summary = WalletSummary {
                detailed_balance: balance
                    .iter()
                    .map(|(k, v)| (hex::encode(&k.to_bytes()), *v))
                    .collect(),
                total_micromel: balance.get(&Denom::Mel).copied().unwrap_or_default(),
                network: self.network,
                address: wallet.address(),
                locked: !self.unlocked_signers.contains_key(&name),
                staked_microsym: Default::default(),
            };
            toret.insert(name, summary);
        }
        toret
    }

    /// Obtains the signer of a wallet. If the wallet is still locked, returns None.
    fn get_signer(&self, name: &str) -> Option<Arc<dyn Signer>> {
        let res = self.unlocked_signers.get(name)?;
        Some(res.clone())
    }

    /// Unlocks a particular wallet. Returns None if unlocking failed.
    fn unlock(&self, name: &str, pwd: Option<String>) -> Option<()> {
        let enc = self.secrets.load(name)?;
        match enc {
            PersistentSecret::Plaintext(sec) => {
                self.unlocked_signers.insert(name.to_owned(), Arc::new(sec));
            }
            PersistentSecret::PasswordEncrypted(enc) => {
                let decrypted = enc.decrypt(&pwd?)?;
                self.unlocked_signers
                    .insert(name.to_owned(), Arc::new(decrypted));
            }
        }
        Some(())
    }

    /// Dumps a particular private key. Use carefully!
    fn get_secret_key(
        &self,
        name: &str,
        pwd: Option<String>,
    ) -> Result<Option<Ed25519SK>, InvalidPassword> {
        let maybe_enc = self.secrets.load(name);
        if let Some(enc) = maybe_enc {
            match enc {
                PersistentSecret::Plaintext(sk) => Ok(Some(sk)),
                PersistentSecret::PasswordEncrypted(enc) => {
                    let decrypted = enc
                        .decrypt(&pwd.ok_or(InvalidPassword)?)
                        .ok_or(InvalidPassword)?;
                    Ok(Some(decrypted))
                }
            }
        } else {
            Ok(None)
        }
    }
    async fn get_wallet(&self, name: &str) -> Option<Box<dyn Melwallet + Send + Sync>> {
        Some(Box::new(self.database.get_wallet(name).await?))
    }
    /// Locks a particular wallet.
    fn lock(&self, name: &str) {
        self.unlocked_signers.remove(name);
    }

    /// Creates a wallet with a given name.
    async fn create_wallet(
        &self,
        name: &str,
        key: Ed25519SK,
        pwd: Option<String>,
    ) -> anyhow::Result<()> {
        let covenant = Covenant::std_ed25519_pk_new(key.to_public());
        self.database.create_wallet(name, covenant).await?;
        self.secrets.store(
            name.to_owned(),
            match pwd {
                Some(pwd) => PersistentSecret::PasswordEncrypted(EncryptedSK::new(key, &pwd)),
                None => PersistentSecret::Plaintext(key),
            },
        );
        log::info!("created wallet with name {}", name);
        Ok(())
    }
}

// task that periodically pulls random coins to try to confirm
async fn confirm_task(database: Database, client: ValClient) {
    let mut pacer = smol::Timer::interval(Duration::from_millis(15000));
    // let sent = Arc::new(Mutex::new(HashMap::new()));
    loop {
        let possible_wallets = database.list_wallets().await;
        log::trace!("-- confirm loop sees {} wallets --", possible_wallets.len());
        match client.snapshot().await {
            Ok(snap) => {
                for wname in possible_wallets {
                    if let Some(wallet) = database.get_wallet(&wname).await {
                        let r = wallet
                            .network_sync(snap.clone())
                            .timeout(Duration::from_secs(120))
                            .await;
                        match r {
                            None => log::warn!("sync {} timed out", wname),
                            Some(Err(err)) => log::warn!("sync {} failed: {:?}", wname, err),
                            _ => (),
                        }
                    }
                }
                let _ = database
                    .retransmit_pending(snap)
                    .timeout(Duration::from_secs(10))
                    .await;
            }
            Err(err) => {
                log::warn!("failed to snap: {:?}", err);
            }
        }
        (&mut pacer).await;
    }
}
