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

use melprot::Client;
use state::AppState;
use tap::Tap;

use clap::Parser;
use tide::{security::CorsMiddleware, Server};

use crate::{
    cli::*,
    protocol::{legacy::route_legacy, route_rpc},
};

use crate::{database::Database, secrets::SecretStore};

use melstructs::NetID;

fn main() -> anyhow::Result<()> {
    let log_conf = std::env::var("RUST_LOG").unwrap_or_else(|_| "melwalletd=debug,warn".into());
    std::env::set_var("RUST_LOG", log_conf);
    env_logger::init();
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

        let client = Client::connect_http(network, addr).await?;

        log::info!("using node RPC {addr}");

        if network == NetID::Mainnet || network == NetID::Testnet {
            client.trust(melbootstrap::checkpoint_height(network).unwrap());
        } else {
            log::warn!("** BLINDLY TRUSTING FULL NODE due to custom network **");
            client.dangerously_trust_latest().await?;
        }

        // Prepare to create server
        let state = AppState::new(db, network, secrets, addr, client);
        let config = Arc::new(config);

        let mut app = init_server(config.clone(), state).await?;

        let sock = config.listen;
        // new RPC interface
        route_rpc(&mut app);
        // old REST-based interface
        route_legacy(&mut app);
        log::info!("starting RPC server at {}", config.listen);
        app.listen(sock).await?;
        Ok(())
    })
}

async fn init_server<T: Send + Sync + Clone + 'static>(
    config: Arc<Config>,
    state: T,
) -> anyhow::Result<Server<T>> {
    let mut app = tide::with_state(state);

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
