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

use crate::{
    cli::*,
    protocol::{legacy::route_legacy, route_rpc},
};

use crate::{database::Database, secrets::SecretStore};
use themelio_nodeprot::ValClient;
use themelio_structs::NetID;

fn main() -> anyhow::Result<()> {
    smolscale::block_on(async {
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

        let log_conf =
            std::env::var("RUST_LOG").unwrap_or_else(|_| "melwalletd=debug,info,warn".into());
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

        let mut app = init_server(config.clone(), state).await?;

        let sock = config.listen;
        // new RPC interface
        route_rpc(&mut app);
        // old REST-based interface
        route_legacy(&mut app);
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

async fn log_request<T>(mut req: Request<T>) -> Request<T> {
    if req.url().path()  != "/" {
        // the path is more than / indicating this may be a legacy endpoint request
        log::info!("{}", req.url());
        return req
    };
    let maybe_body = req.body_string().await;
    let Ok(body) = maybe_body else {
        log::warn!("IO error or unable to decode body as UTF-8");
        return req
    };
    req.set_body(body.clone());
    let maybe_json_req: Result<nanorpc::JrpcRequest, _> = serde_json::from_str(&body);
    let Ok(json_req) = maybe_json_req else {
        log::warn!("Body isn't shaped like nanorpc::JrpcRequest");
        return req;
    };

    // if debug mode is enabled, output the whole request
    if log::log_enabled!(log::Level::Debug) {
        log::debug!(
            "{}: {}",
            json_req.method,
            serde_json::to_string_pretty(&json_req.params).unwrap()
        );
    } 
    // else just output the method
    else {
        log::info!("{}", json_req.method,);
    }
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
