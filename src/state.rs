use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use crate::{
    database::{Database, Wallet},
    secrets::{EncryptedSK, PersistentSecret, SecretStore},
    signer::Signer,
    walletdata::LegacyWalletData,
};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use smol::future::FutureExt;
use smol_timeout::TimeoutExt;
use themelio_nodeprot::ValClient;
use themelio_stf::melvm::Covenant;
use themelio_structs::{Address, CoinValue, Denom, NetID};
use tmelcrypt::Ed25519SK;

/// Encapsulates all the state and logic needed for the wallet daemon.
pub struct AppState {
    mainnet_db: Database,
    testnet_db: Database,
    clients: HashMap<NetID, ValClient>,
    unlocked_signers: DashMap<String, Arc<dyn Signer>>,
    secrets: SecretStore,
    _confirm_task: smol::Task<()>,
}

impl AppState {
    /// Creates a new appstate, given a mainnet and testnet server.
    pub fn new(
        mainnet_db: Database,
        testnet_db: Database,
        secrets: SecretStore,
        mainnet_addr: SocketAddr,
        testnet_addr: SocketAddr,
    ) -> Self {
        let mainnet_client = ValClient::new(NetID::Mainnet, mainnet_addr);
        let testnet_client = ValClient::new(NetID::Testnet, testnet_addr);
        mainnet_client.trust(themelio_bootstrap::checkpoint_height(NetID::Mainnet).unwrap());
        testnet_client.trust(themelio_bootstrap::checkpoint_height(NetID::Testnet).unwrap());
        let clients: HashMap<NetID, ValClient> = vec![
            (NetID::Mainnet, mainnet_client.clone()),
            (NetID::Testnet, testnet_client.clone()),
        ]
        .into_iter()
        .collect();

        let _confirm_task = smolscale::spawn(
            confirm_task(mainnet_db.clone(), mainnet_client)
                .race(confirm_task(testnet_db.clone(), testnet_client)),
        );

        Self {
            mainnet_db,
            testnet_db,
            clients,
            unlocked_signers: Default::default(),
            secrets,
            _confirm_task,
        }
    }

    /// Returns a summary of wallets.
    pub async fn list_wallets(&self) -> BTreeMap<String, WalletSummary> {
        let mlist = self.mainnet_db.list_wallets().await;
        let tlist = self.testnet_db.list_wallets().await;
        let mut toret = BTreeMap::new();
        for name in mlist.into_iter().chain(tlist.into_iter()) {
            let (wallet, network) = self.get_wallet(&name).await.unwrap();
            let balance = wallet.get_balances().await;
            let summary = WalletSummary {
                detailed_balance: balance
                    .iter()
                    .map(|(k, v)| (hex::encode(&k.to_bytes()), *v))
                    .collect(),
                total_micromel: balance.get(&Denom::Mel).copied().unwrap_or_default(),
                network,
                address: wallet.address(),
                locked: !self.unlocked_signers.contains_key(&name),
                staked_microsym: Default::default(),
            };
            toret.insert(name, summary);
        }
        toret
    }

    /// Returns a single summary of a wallet.
    pub async fn wallet_summary(&self, name: &str) -> Option<WalletSummary> {
        self.list_wallets().await.get(name).cloned()
    }

    /// Obtains the signer of a wallet. If the wallet is still locked, returns None.
    pub fn get_signer(&self, name: &str) -> Option<Arc<dyn Signer>> {
        let res = self.unlocked_signers.get(name)?;
        Some(res.clone())
    }

    /// Unlocks a particular wallet. Returns None if unlocking failed.
    pub fn unlock(&self, name: &str, pwd: Option<String>) -> Option<()> {
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
    pub fn get_secret_key(&self, name: &str, pwd: Option<String>) -> Option<Ed25519SK> {
        let enc = self.secrets.load(name)?;
        match enc {
            PersistentSecret::Plaintext(sk) => Some(sk),
            PersistentSecret::PasswordEncrypted(enc) => {
                let decrypted = enc.decrypt(&pwd?)?;
                Some(decrypted)
            }
        }
    }

    /// Locks a particular wallet.
    pub fn lock(&self, name: &str) {
        self.unlocked_signers.remove(name);
    }

    /// Gets a wallet by name, returning the wallet handle and what network it belongs to.
    pub async fn get_wallet(&self, name: &str) -> Option<(Wallet, NetID)> {
        if let Some(wallet) = self.mainnet_db.get_wallet(name).await {
            Some((wallet, NetID::Mainnet))
        } else {
            self.testnet_db
                .get_wallet(name)
                .await
                .map(|wallet| (wallet, NetID::Testnet))
        }
    }

    /// Creates a wallet with a given name.
    pub async fn create_wallet(
        &self,
        name: &str,
        network: NetID,
        key: Ed25519SK,
        pwd: Option<String>,
    ) -> anyhow::Result<()> {
        let covenant = Covenant::std_ed25519_pk_new(key.to_public());
        self.database(network).create_wallet(name, covenant).await?;
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

    /// Gets a reference to the client.
    pub fn client(&self, network: NetID) -> &ValClient {
        &self.clients[&network]
    }

    /// Gets a reference to the database.
    pub fn database(&self, network: NetID) -> &Database {
        if network == NetID::Mainnet {
            &self.mainnet_db
        } else {
            &self.testnet_db
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletSummary {
    pub total_micromel: CoinValue,
    pub detailed_balance: BTreeMap<String, CoinValue>,
    pub staked_microsym: CoinValue,
    pub network: NetID,
    #[serde(with = "stdcode::asstr")]
    pub address: Address,
    pub locked: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletDump {
    pub summary: WalletSummary,
    pub full: LegacyWalletData,
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
        // log::debug!("-- confirm loop sees {} wallets --", possible_wallets.len());
        // for wallet_name in possible_wallets {
        //     let wallet = multi.get_wallet(&wallet_name);
        //     match wallet {
        //         Err(err) => {
        //             log::error!("cannot read wallet: {}", err);
        //         }
        //         Ok(wallet) => {
        //             let client = clients[&wallet.read().network()].clone();
        //             smolscale::spawn(confirm_one(wallet_name, wallet, client, sent.clone()))
        //                 .detach()
        //         }
        //     }
        // }
        (&mut pacer).await;
    }
}
