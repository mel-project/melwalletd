mod cli;
mod database;
mod protocol;
mod secrets;
mod state;
use std::convert::TryFrom;

use std::{ffi::CString, sync::Arc};

use anyhow::Context;

use database::Wallet;
use melwalletd_prot::protocol::MelwalletdService;
use protocol::{MelwalletdRpcImpl};
use state::AppState;
use tap::Tap;

use clap::{Parser};

use crate::cli::*;
// use crate::protocol::legacy::melwalletd_http_server;
use crate::{database::Database, secrets::SecretStore};
use themelio_nodeprot::ValClient;
use themelio_structs::NetID;


fn main() -> anyhow::Result<()> {
    smolscale::block_on(async {
        // let clap = __clap;
        let cmd_args = Args::from_args();
        let output_config = cmd_args.output_config;
        let dry_run = cmd_args.dry_run;

        let config = Config::try_from(cmd_args).expect("Unable to create config from cmd args");
        let network = config.network;
        let addr = config.network_addr;
        let db_name = format!("{network:?}-wallets.db").to_ascii_lowercase();

        if output_config {
            println!(
                "{}",
                serde_yaml::to_string(&config)
                    .expect("Critical Failure: Unable to serialize `Config`")
            );
        }

        if dry_run {
            return Ok(());
        }

        std::fs::create_dir_all(&config.wallet_dir).context("cannot create wallet_dir")?;
        // SAFETY: this is perfectly safe because chmod cannot lead to memory unsafety.
        unsafe {
            libc::chmod(
                CString::new(config.wallet_dir.to_string_lossy().as_bytes().to_vec())?.as_ptr(),
                0o700,
            );
        }

        let db = Database::open(config.wallet_dir.clone().tap_mut(|p| p.push(db_name))).await?;

        let mut secret_path = config.wallet_dir.clone();
        secret_path.push(".secrets.json");
        let secrets = SecretStore::open(&secret_path)?;

        let log_conf = std::env::var("RUST_LOG").unwrap_or_else(|_| "melwalletd=debug,warn".into());
        std::env::set_var("RUST_LOG", log_conf);
        tracing_subscriber::fmt::init();

        let client = ValClient::new(network, addr);
        if network == NetID::Mainnet || network == NetID::Testnet {
            client.trust(themelio_bootstrap::checkpoint_height(network).unwrap());
        } else {
            log::warn!("** BLINDLY TRUSTING FULL NODE due to custom network **");
            client.insecure_latest_snapshot().await?;
        }

        // Prepare to create server
        let state = Arc::new(AppState::new(db, network, secrets, addr, client));
        let config = Arc::new(config);
        type WalletType = MelwalletdRpcImpl<AppState>;
        
        let task = match config.legacy_listen {
            Some(sock) => {
                let rpc: WalletType = MelwalletdRpcImpl::new(state.clone());

                let app =
                    crate::protocol::legacy::init_server(config.clone(), rpc).await?;
                let legacy_endpoints = crate::protocol::legacy::legacy_server(app)?;
                let server = legacy_endpoints.listen(sock);
                log::info!("Starting legacy server at {}", sock);
                Some(smolscale::spawn(server))
            }
            _ => None
        };
    
        {
            let rpc: WalletType = MelwalletdRpcImpl::new(state.clone());
            let service = MelwalletdService(rpc);

            let app =
                crate::protocol::legacy::init_server(config.clone(), service).await?;
            
            let sock = config.listen;
            let legacy = crate::protocol::legacy::rpc_server(app).await?;
            log::info!("Starting rpc server at {}", config.listen);
            legacy.listen(sock).await?
        };

        Ok(())
    })
}
