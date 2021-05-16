use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    time::Duration,
};

use crate::{acidjson::AcidJson, multi::MultiWallet, walletdata::WalletData};
use anyhow::Context;
use blkstructs::{melvm::Covenant, CoinDataHeight, CoinID, Denom, NetID, Transaction};
use nanorand::RNG;
use nodeprot::ValClient;
use serde::{Deserialize, Serialize};
use tmelcrypt::{Ed25519SK, HashVal};

/// Encapsulates all the state and logic needed for the wallet daemon.
pub struct AppState {
    multi: MultiWallet,
    clients: HashMap<NetID, ValClient>,
    _confirm_task: smol::Task<()>,
}

impl AppState {
    /// Creates a new appstate, given a mainnet and testnet server.
    pub fn new(multi: MultiWallet, mainnet_addr: SocketAddr, testnet_addr: SocketAddr) -> Self {
        let mainnet_client = ValClient::new(NetID::Mainnet, mainnet_addr);
        let testnet_client = ValClient::new(NetID::Testnet, testnet_addr);
        mainnet_client.trust(
            14146,
            "50f5a41c6e996d36bc05b1272a59c8adb3fe3f98de70965abd2eed0c115d2108"
                .parse()
                .unwrap(),
        );
        testnet_client.trust(
            2550,
            "2b2133e34779c4043278a5d084671a7a801022605dba2721e2d164d9c1096c13"
                .parse()
                .unwrap(),
        );
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
                    .filter(|(_, cdh)| cdh.coin_data.denom == Denom::Mel)
                    .map(|(_, cdh)| cdh.coin_data.value)
                    .sum();
                (
                    name,
                    WalletSummary {
                        total_micromel,
                        network: wd.network(),
                        address: wd.my_covenant().hash().to_addr(),
                    },
                )
            })
            .collect()
    }

    /// Dumps the state of a particular wallet.
    pub fn dump_wallet(&self, name: &str) -> Option<WalletDump> {
        let summary = self.list_wallets().get(name)?.clone();
        let full = self.multi.get_wallet(name).ok()?.read().clone();
        Some(WalletDump { summary, full })
    }

    /// Creates a wallet with a given name. If the wallet was successfully created, return its secret key.
    pub fn create_wallet(&self, name: &str, network: NetID) -> Option<Ed25519SK> {
        if self.list_wallets().contains_key(name) {
            return None;
        }
        let (pk, sk) = tmelcrypt::ed25519_keygen();
        let covenant = Covenant::std_ed25519_pk_new(pk);
        self.multi.create_wallet(name, covenant, network).ok()?;
        Some(sk)
    }

    /// Gets a reference to the inner stuff.
    pub fn multi(&self) -> &MultiWallet {
        &self.multi
    }

    /// Gets a reference to the client.
    pub fn client(&self, network: NetID) -> &ValClient {
        &self.clients[&network]
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletSummary {
    pub total_micromel: u128,
    pub network: NetID,
    pub address: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletDump {
    pub summary: WalletSummary,
    pub full: WalletData,
}

// task that periodically pulls random coins to try to confirm
async fn confirm_task(multi: MultiWallet, clients: HashMap<NetID, ValClient>) {
    let mut pacer = smol::Timer::interval(Duration::from_secs(1));
    loop {
        (&mut pacer).await;
        let possible_wallets = multi.list().collect::<Vec<_>>();
        if possible_wallets.is_empty() {
            continue;
        }
        let wallet_name =
            &possible_wallets[nanorand::tls_rng().generate_range(0, possible_wallets.len())];
        let wallet = multi.get_wallet(&wallet_name);
        match wallet {
            Err(err) => {
                log::error!("could not read wallet: {:?}", err);
                continue;
            }
            Ok(wallet) => {
                let client = clients[&wallet.read().network()].clone();
                smolscale::spawn(async move {
                    if let Err(err) = confirm_one(wallet, client).await {
                        log::warn!("error in confirm_one: {:?}", err)
                    }
                })
                .detach();
            }
        }
    }
}

async fn confirm_one(wallet: AcidJson<WalletData>, client: ValClient) -> anyhow::Result<()> {
    let in_progress: BTreeMap<HashVal, Transaction> = wallet.read().tx_in_progress().clone();
    // we pick a random value that's in progress
    let in_progress: Vec<Transaction> = in_progress.values().cloned().collect();
    if in_progress.is_empty() {
        return Ok(());
    }
    let snapshot = client.snapshot().await.context("cannot snapshot")?;
    let random_tx = &in_progress[nanorand::tls_rng().generate_range(0, in_progress.len())];
    if nanorand::tls_rng().generate_range(0u8, 10) == 0 {
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
            let coin_id = random_tx.get_coinid(change_idx as u8);
            log::debug!("confirm_one looking at {}", coin_id);
            let cdh = snapshot
                .get_coin(random_tx.get_coinid(change_idx as u8))
                .await
                .context("cannot get coin")?;
            if let Some(cdh) = cdh {
                log::debug!("confirmed {} at height {}", coin_id, cdh.height);
                wallet.write().insert_coin(coin_id, cdh);
            } else {
                log::debug!("{} not confirmed yet", coin_id);
                break;
            }
        }
        Ok(())
    }
}
