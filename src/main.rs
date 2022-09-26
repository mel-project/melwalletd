mod cli;
mod database;
mod secrets;
mod signer;
mod state;
mod wallet_utils;
mod walletdata;
use std::convert::TryFrom;

use std::{ ffi::CString, sync::Arc};

use anyhow::Context;

use http_types::headers::HeaderValue;
use state::AppState;
use tap::Tap;

use clap::Parser;

use themelio_nodeprot::ValClient;
use themelio_structs::{
     NetID
};
use tide::security::CorsMiddleware;
use tide::{Request};

use crate::cli::*;
use crate::{database::Database, secrets::SecretStore};
use crate::wallet_utils::*;

fn generate_cors(origins: Vec<String>) -> CorsMiddleware {
    let cors = origins
        .iter()
        .fold(CorsMiddleware::new(), |cors, val| {
            let s: &str = val;
            cors.allow_origin(s)
        })
        .allow_methods("GET, POST, PUT".parse::<HeaderValue>().unwrap())
        .allow_credentials(false);

    cors
}

fn main() -> anyhow::Result<()> {
    smolscale::block_on(async {
        let log_conf = std::env::var("RUST_LOG").unwrap_or_else(|_| "melwalletd=debug,warn".into());
        std::env::set_var("RUST_LOG", log_conf);
        tracing_subscriber::fmt::init();

        // let clap = __clap;
        let cmd_args = Args::from_args();

        let output_config = cmd_args.output_config;
        let dry_run = cmd_args.dry_run;

        let config = match Config::try_from(cmd_args) {
            Ok(i) => anyhow::Ok(i),
            Err(err) => {
                let fmt = format!("Configuration Error: {}", err);
                return Err(anyhow::anyhow!(fmt));
            }
        }?;

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

        let client = ValClient::new(network, addr);
        if network == NetID::Mainnet || network == NetID::Testnet {
            client.trust(themelio_bootstrap::checkpoint_height(network).unwrap());
        } else {
            log::warn!("** BLINDLY TRUSTING FULL NODE due to custom network **");
            client.insecure_latest_snapshot().await?;
        }

        let mut secret_path = config.wallet_dir.clone();
        secret_path.push(".secrets.json");
        let secrets = SecretStore::open(&secret_path)?;

        let state = AppState::new(db, network, secrets, addr, client);

        let mut app = tide::with_state(Arc::new(state));

        async fn log_request<T>(req: Request<T>) -> Request<T> {
            log::info!("{}", req.url());
            req
        }
        app.with(tide::utils::Before(log_request));

        // interpret errors
        app.with(tide::utils::After(|mut res: tide::Response| async move {
            if let Some(err) = res.error() {
                // put the error string in the response
                let err_str = format!("ERROR: {:?}", err);
                log::warn!("{}", err_str);
                res.set_body(err_str);
            }
            Ok(res)
        }));
        app.at("/summary").get(get_summary);
        app.at("/pools/:pair").get(get_pool);
        app.at("/pool_info").post(get_pool_info);
        app.at("/wallets").get(list_wallets);
        app.at("/wallets/:name").get(summarize_wallet);
        app.at("/wallets/:name").put(create_wallet);
        app.at("/wallets/:name/lock").post(lock_wallet);
        app.at("/wallets/:name/unlock").post(unlock_wallet);
        app.at("/wallets/:name/export-sk")
            .post(export_sk_from_wallet);
        app.at("/wallets/:name/coins").get(dump_coins);
        app.at("/wallets/:name/prepare-tx").post(prepare_tx);
        app.at("/wallets/:name/send-tx").post(send_tx);
        app.at("/wallets/:name/send-faucet").post(send_faucet);
        app.at("/wallets/:name/transactions").get(dump_transactions);
        app.at("/wallets/:name/transactions/:txhash").get(get_tx);
        app.at("/wallets/:name/transactions/:txhash/balance")
            .get(get_tx_balance);
        let cors = generate_cors(config.allowed_origins);

        app.with(cors);

        log::info!("Starting server at {}", config.listen);
        app.listen(config.listen).await?;

        Ok(())
    })
}


