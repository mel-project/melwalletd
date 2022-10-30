mod cli;
mod database;
mod protocol;
mod secrets;
mod signer;
mod state;
use std::convert::TryFrom;

use std::{ffi::CString, sync::Arc};

use anyhow::Context;

use http_types::headers::HeaderValue;

use state::AppState;
use tap::Tap;

use clap::Parser;
use tide::{security::CorsMiddleware, Request, Server};

use crate::cli::*;

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

        let client = ValClient::connect_melnet2_tcp(network, addr).await?;

        log::info!("Connecting to Node rpc @ {addr}");

        if network == NetID::Mainnet || network == NetID::Testnet {
            client.trust(themelio_bootstrap::checkpoint_height(network).unwrap());
        } else {
            log::warn!("** BLINDLY TRUSTING FULL NODE due to custom network **");
            client.insecure_latest_snapshot().await?;
        }

        // Prepare to create server
        let state = AppState::new(db, network, secrets, addr, client);
        let config = Arc::new(config);

        let _task = match config.legacy_listen {
            Some(sock) => {
                let app = init_server(config.clone(), state.clone()).await?;
                let legacy_endpoints = crate::protocol::legacy::legacy_server(app)?;
                let server = legacy_endpoints.listen(sock);
                log::info!("Starting legacy server at {}", sock);
                let task = smolscale::spawn(async move {
                    let s = server.await;
                    match s {
                        Ok(_) => (),
                        Err(e) => {
                            log::error!("{}", e);
                            panic!("{}", e)
                        }
                    };
                    log::info!("Legacy server terminated");
                });
                Some(task)
            }
            _ => None,
        };

        let mut app = init_server(config.clone(), state).await?;

        let sock = config.listen;
        crate::protocol::route_rpc(&mut app).await;
        log::info!("Starting rpc server at {}", config.listen);
        app.listen(sock).await?;
        Ok(())
    })
}

async fn init_server<T: Send + Sync + Clone + 'static>(
    config: Arc<Config>,
    state: T,
) -> anyhow::Result<Server<T>> {
    let mut app = tide::with_state(state);

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

    let cors = generate_cors(config.allowed_origins.clone());

    app.with(cors);

    Ok(app)
}

async fn log_request<T>(req: Request<T>) -> Request<T> {
    log::info!("{}", req.url());
    req
}

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
