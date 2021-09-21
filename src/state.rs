use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use crate::{
    multi::MultiWallet,
    secrets::{EncryptedSK, PersistentSecret, SecretStore},
    signer::Signer,
    to_badgateway,
    walletdata::WalletData,
};
use acidjson::AcidJson;
use anyhow::Context;
use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use themelio_nodeprot::{ValClient, InMemoryTrustStore};
use themelio_stf::{
    melvm::{Address, Covenant},
    CoinDataHeight, CoinID, Denom, NetID, Transaction, TxHash,
};
use tmelcrypt::Ed25519SK;

/// Encapsulates all the state and logic needed for the wallet daemon.
pub struct AppState {
    multi: MultiWallet,
    clients: HashMap<NetID, ValClient>,
    unlocked_signers: DashMap<String, Arc<dyn Signer>>,
    secrets: SecretStore,
    _confirm_task: smol::Task<()>,
}

impl AppState {
    /// Creates a new appstate, given a mainnet and testnet server.
    pub fn new(
        multi: MultiWallet,
        secrets: SecretStore,
        trusted_blocks: InMemoryTrustStore,
        mainnet_addr: SocketAddr,
        testnet_addr: SocketAddr,
    ) -> Self {
        let mainnet_client = ValClient::new(NetID::Mainnet, mainnet_addr, trusted_blocks.clone());
        let testnet_client = ValClient::new(NetID::Testnet, testnet_addr, trusted_blocks);

        let clients: HashMap<NetID, ValClient> = vec![
            (NetID::Mainnet, mainnet_client),
            (NetID::Testnet, testnet_client),
        ]
        .into_iter()
        .collect();

        let _confirm_task = smolscale::spawn(confirm_task(multi.clone(), clients.clone()));

        Self {
            multi,
            clients,
            unlocked_signers: Default::default(),
            secrets,
            _confirm_task,
        }
    }

    /// Returns a summary of wallets.
    pub fn list_wallets(&self) -> BTreeMap<String, WalletSummary> {
        self.multi
            .list()
            .filter_map(|v| self.multi.get_wallet(&v).ok().map(|wd| (v, wd)))
            .map(|(name, wd)| {
                let wd = wd.read();
                let unspent: &BTreeMap<CoinID, CoinDataHeight> = wd.unspent_coins();
                let total_micromel = unspent
                    .iter()
                    .filter(|(_, cdh)| {
                        cdh.coin_data.denom == Denom::Mel
                            && cdh.coin_data.covhash == wd.my_covenant().hash()
                    })
                    .map(|(_, cdh)| cdh.coin_data.value)
                    .sum();
                let mut detailed_balance = BTreeMap::new();
                for (_, cdh) in unspent.iter() {
                    let entry = detailed_balance
                        .entry(hex::encode(&cdh.coin_data.denom.to_bytes()))
                        .or_default();
                    if cdh.coin_data.covhash == wd.my_covenant().hash() {
                        *entry += cdh.coin_data.value;
                    }
                }
                let staked_microsym = wd
                    .stake_list()
                    .values()
                    .map(|v| v.syms_staked)
                    .sum::<u128>();
                let locked = !self.unlocked_signers.contains_key(&name);
                (
                    name,
                    WalletSummary {
                        total_micromel,
                        detailed_balance,
                        network: wd.network(),
                        address: wd.my_covenant().hash(),
                        locked,
                        staked_microsym,
                    },
                )
            })
            .collect()
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

    /// Locks a particular wallet.
    pub fn lock(&self, name: &str) {
        self.unlocked_signers.remove(name);
    }

    /// Dumps the state of a particular wallet.
    pub fn dump_wallet(&self, name: &str) -> Option<WalletDump> {
        let summary = self.list_wallets().get(name)?.clone();
        let full = self.multi.get_wallet(name).ok()?.read().clone();
        Some(WalletDump { summary, full })
    }

    /// Replaces the content of some wallet, wholesale.
    pub fn insert_wallet(&self, name: &str, dump: WalletData) {
        let _ = self
            .multi
            .create_wallet(name, dump.my_covenant().clone(), dump.network());
        let wallet = self
            .multi
            .get_wallet(name)
            .expect("this does not make any sense");
        *wallet.write() = dump;
    }

    /// Creates a wallet with a given name.
    pub fn create_wallet(
        &self,
        name: &str,
        network: NetID,
        key: Ed25519SK,
        pwd: Option<String>,
    ) -> Option<()> {
        if self.list_wallets().contains_key(name) {
            return None;
        }
        let covenant = Covenant::std_ed25519_pk_new(key.to_public());
        self.multi.create_wallet(name, covenant, network).ok()?;
        self.secrets.store(
            name.to_owned(),
            match pwd {
                Some(pwd) => PersistentSecret::PasswordEncrypted(EncryptedSK::new(key, &pwd)),
                None => PersistentSecret::Plaintext(key),
            },
        );
        Some(())
    }

    /// Gets a reference to the inner stuff.
    pub fn multi(&self) -> &MultiWallet {
        &self.multi
    }

    /// Gets a reference to the client.
    pub fn client(&self, network: NetID) -> &ValClient {
        &self.clients[&network]
    }

    /// Calculates the current fee multiplier, given the network.
    pub async fn current_fee_multiplier(&self, network: NetID) -> tide::Result<u128> {
        let client = self.client(network).clone();
        let snapshot = client.snapshot().await.map_err(to_badgateway)?;
        Ok(snapshot.current_header().fee_multiplier)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletSummary {
    pub total_micromel: u128,
    pub detailed_balance: BTreeMap<String, u128>,
    pub staked_microsym: u128,
    pub network: NetID,
    #[serde(with = "stdcode::asstr")]
    pub address: Address,
    pub locked: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletDump {
    pub summary: WalletSummary,
    pub full: WalletData,
}

// task that periodically pulls random coins to try to confirm
async fn confirm_task(multi: MultiWallet, clients: HashMap<NetID, ValClient>) {
    let mut pacer = smol::Timer::interval(Duration::from_millis(100));
    let sent = Arc::new(Mutex::new(HashSet::new()));
    loop {
        (&mut pacer).await;
        let possible_wallets = multi.list().collect::<Vec<_>>();
        if possible_wallets.is_empty() {
            continue;
        }
        let wallet_name = &possible_wallets[fastrand::usize(0..possible_wallets.len())];
        let wallet = multi.get_wallet(wallet_name);
        match wallet {
            Err(err) => {
                log::error!("could not read wallet: {:?}", err);
                continue;
            }
            Ok(wallet) => {
                let client = clients[&wallet.read().network()].clone();
                let sent = sent.clone();
                smolscale::spawn(async move {
                    if let Err(err) = confirm_one(wallet, client, sent).await {
                        log::warn!("error in confirm_one: {:?}", err)
                    }
                })
                .detach();
            }
        }
    }
}

async fn confirm_one(
    wallet: AcidJson<WalletData>,
    client: ValClient,
    sent: Arc<Mutex<HashSet<TxHash>>>,
) -> anyhow::Result<()> {
    let snapshot = client.snapshot().await.context("cannot snapshot")?;
    wallet
        .write()
        .retain_valid_stakes(snapshot.current_header().height);
    let in_progress: BTreeMap<TxHash, Transaction> = wallet.read().tx_in_progress().clone();
    // we pick a random value that's in progress
    let in_progress: Vec<Transaction> = in_progress.values().cloned().collect();
    if in_progress.is_empty() {
        return Ok(());
    }
    let random_tx = &in_progress[fastrand::usize(0..in_progress.len())];
    if fastrand::u8(0u8..10) == 0 || sent.lock().insert(random_tx.hash_nosigs()) {
        if let Err(err) = snapshot.get_raw().send_tx(random_tx.clone()).await {
            log::debug!(
                "retransmission of {} saw error: {:?}",
                random_tx.hash_nosigs(),
                err
            );
        }
        Ok(())
    } else {
        // find some change output
        let my_covhash = wallet.read().my_covenant().hash();
        let change_indexes = random_tx
            .outputs
            .iter()
            .enumerate()
            .filter(|v| v.1.covhash == my_covhash)
            .map(|v| v.0);
        for change_idx in change_indexes {
            let coin_id = random_tx.output_coinid(change_idx as u8);
            log::trace!("confirm_one looking at {}", coin_id);
            let cdh = snapshot
                .get_coin(random_tx.output_coinid(change_idx as u8))
                .await
                .context("cannot get coin")?;
            if let Some(cdh) = cdh {
                log::debug!("confirmed {} at height {}", coin_id, cdh.height);
                wallet.write().insert_coin(coin_id, cdh);
                sent.lock().remove(&random_tx.hash_nosigs());
            } else {
                log::debug!("{} not confirmed yet", coin_id);
                break;
            }
        }
        Ok(())
    }
}
