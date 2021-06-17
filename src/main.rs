mod multi;
mod secrets;
mod signer;
mod state;
mod walletdata;
use std::{collections::BTreeMap, ffi::CString, net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use http_types::headers::HeaderValue;
use multi::MultiWallet;
use nanorand::RNG;
use serde::Deserialize;
use state::AppState;
use std::fmt::Debug;
use structopt::StructOpt;
use themelio_stf::{CoinData, CoinID, Denom, NetID, Transaction, TxHash, TxKind, MICRO_CONVERTER};
use tide::security::CorsMiddleware;
use tide::{Body, Request, StatusCode};
use tmelcrypt::Ed25519SK;

use crate::{secrets::SecretStore, signer::Signer};

#[derive(StructOpt)]
struct Args {
    #[structopt(long)]
    wallet_dir: PathBuf,

    #[structopt(long, default_value = "127.0.0.1:11773")]
    listen: SocketAddr,

    #[structopt(long, default_value = "51.83.255.223:11814")]
    mainnet_connect: SocketAddr,

    #[structopt(long, default_value = "94.237.109.44:11814")]
    testnet_connect: SocketAddr,
}

fn main() -> anyhow::Result<()> {
    smolscale::block_on(async {
        let log_conf = std::env::var("RUST_LOG").unwrap_or_else(|_| "melwalletd=debug,warn".into());
        std::env::set_var("RUST_LOG", log_conf);
        tracing_subscriber::fmt::init();
        let args = Args::from_args();
        std::fs::create_dir_all(&args.wallet_dir).context("cannot create wallet_dir")?;
        // SAFETY: this is perfectly safe because chmod cannot lead to memory unsafety.
        unsafe {
            libc::chmod(
                CString::new(args.wallet_dir.to_string_lossy().as_bytes().to_vec())?.as_ptr(),
                0o700,
            );
        }
        let multiwallet = MultiWallet::open(&args.wallet_dir)?;
        log::info!(
            "opened wallet directory: {:?}",
            multiwallet.list().collect::<Vec<_>>()
        );
        let mut secret_path = args.wallet_dir.clone();
        secret_path.push(".secrets.json");
        let secrets = SecretStore::open(&secret_path)?;

        let state = AppState::new(
            multiwallet,
            secrets,
            args.mainnet_connect,
            args.testnet_connect,
        );

        let mut app = tide::with_state(Arc::new(state));
        // set CORS
        app.with(
            CorsMiddleware::new()
                .allow_methods("GET, POST, PUT, OPTIONS".parse::<HeaderValue>().unwrap()),
        );
        app.with(tide::utils::After(|mut res: tide::Response| async move {
            if let Some(err) = res.error() {
                // put the error string in the response
                let err_str = format!("ERROR: {:?}", err);
                log::warn!("{}", err_str);
                res.set_body(err_str);
            }
            Ok(res)
        }));
        // interpret errors
        app.at("/wallets").get(list_wallets);
        app.at("/wallets/:name").get(dump_wallet);
        app.at("/wallets/:name").put(create_wallet);
        app.at("/wallets/:name/unlock").post(unlock_wallet);
        app.at("/wallets/:name/coins/:coinid").put(add_coin);
        app.at("/wallets/:name/prepare-tx").post(prepare_tx);
        app.at("/wallets/:name/send-tx").post(send_tx);
        app.at("/wallets/:name/send-faucet").post(send_faucet);
        app.at("/wallets/:name/transactions/:txhash").get(get_tx);
        app.listen(args.listen).await?;
        Ok(())
    })
}

async fn list_wallets(req: Request<Arc<AppState>>) -> tide::Result<Body> {
    Body::from_json(&smol::unblock(move || req.state().list_wallets()).await)
}

async fn create_wallet(mut req: Request<Arc<AppState>>) -> tide::Result<Body> {
    #[derive(Deserialize)]
    struct Query {
        testnet: bool,
    }
    let query: Query = req.body_json().await?;
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    Body::from_json(&hex::encode(
        &req.state()
            .create_wallet(
                &wallet_name,
                if query.testnet {
                    NetID::Testnet
                } else {
                    NetID::Mainnet
                },
            )
            .context("cannot create wallet")?
            .0,
    ))
}

async fn dump_wallet(req: Request<Arc<AppState>>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let query: BTreeMap<String, String> = req.query()?;
    if query.contains_key("summary") {
        Body::from_json(
            &req.state()
                .dump_wallet(&wallet_name)
                .ok_or_else(notfound)?
                .summary,
        )
    } else {
        Body::from_json(&req.state().dump_wallet(&wallet_name).ok_or_else(notfound)?)
    }
}

async fn add_coin(req: Request<Arc<AppState>>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let coin_id: CoinID = req.param("coinid")?.parse().map_err(to_badreq)?;
    let wallet = req
        .state()
        .multi()
        .get_wallet(&wallet_name)
        .map_err(to_badreq)?;
    // Get the stuff
    let client = req.state().client(wallet.read().network()).clone();
    let snapshot = client.snapshot().await.map_err(to_badgateway)?;
    let cdh = snapshot
        .get_coin(coin_id)
        .await
        .map_err(to_badgateway)?
        .ok_or_else(notfound)?;
    smol::unblock(move || wallet.write().insert_coin(coin_id, cdh)).await;
    Ok(Body::from_string("".into()))
}

async fn unlock_wallet(mut req: Request<Arc<AppState>>) -> tide::Result<Body> {
    #[derive(Deserialize)]
    struct Req {
        pwd: Option<String>,
    }
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let request: Req = req.body_json().await?;
    // attempt to unlock
    req.state()
        .unlock_signer(&wallet_name, request.pwd)
        .context("incorrect password")
        .map_err(to_forbidden)?;
    Ok("".into())
}

async fn prepare_tx(mut req: Request<Arc<AppState>>) -> tide::Result<Body> {
    #[derive(Deserialize)]
    struct Req {
        outputs: Vec<CoinData>,
        signing_key: Option<String>,
        kind: Option<TxKind>,
        data: Option<String>,
    }
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let request: Req = req.body_json().await?;
    let signing_key: Arc<dyn Signer> = if let Some(signing_key) = request.signing_key.as_ref() {
        Arc::new(signing_key.parse::<Ed25519SK>()?)
    } else {
        req.state()
            .get_signer(&&wallet_name)
            .context("wallet is locked")
            .map_err(to_forbidden)?
    };
    let wallet = req
        .state()
        .multi()
        .get_wallet(&wallet_name)
        .map_err(to_badreq)?;

    // calculate fees
    let client = req.state().client(wallet.read().network()).clone();
    let snapshot = client.snapshot().await.map_err(to_badgateway)?;
    let fee_multiplier = snapshot.current_header().fee_multiplier;
    let kind = request.kind;
    let data = match request.data.as_ref() {
        Some(v) => Some(hex::decode(v).map_err(to_badreq)?),
        None => None,
    };
    let prepared_tx = smol::unblock(move || {
        wallet
            .read()
            .prepare(request.outputs, fee_multiplier, |mut tx: Transaction| {
                if let Some(kind) = kind {
                    tx.kind = kind
                }
                if let Some(data) = data.clone() {
                    tx.data = data
                }
                for i in 0..tx.inputs.len() {
                    tx = signing_key.sign_tx(tx, i)?;
                }
                Ok(tx)
            })
    })
    .await
    .map_err(to_badreq)?;

    Ok(Body::from_json(&prepared_tx)?)
}

async fn send_tx(mut req: Request<Arc<AppState>>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let tx: Transaction = req.body_json().await?;
    let wallet = req
        .state()
        .multi()
        .get_wallet(&wallet_name)
        .map_err(to_badreq)?;
    // we mark the TX as sent in this thread. confirmer will send it off later.
    wallet.write().commit_sent(tx.clone()).map_err(to_badreq)?;
    Ok(Body::from_json(&tx.hash_nosigs())?)
}

async fn get_tx(req: Request<Arc<AppState>>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let txhash: TxHash = TxHash(req.param("txhash")?.parse().map_err(to_badreq)?);
    let wallet = req
        .state()
        .multi()
        .get_wallet(&wallet_name)
        .map_err(to_badreq)?;
    let txstatus = wallet.read().get_tx_status(txhash).ok_or_else(notfound)?;
    Ok(Body::from_json(&txstatus)?)
}

async fn send_faucet(req: Request<Arc<AppState>>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let wallet = req
        .state()
        .multi()
        .get_wallet(&wallet_name)
        .map_err(to_badreq)?;
    if wallet.read().network() != NetID::Testnet {
        return Err(tide::Error::new(
            StatusCode::BadRequest,
            anyhow::anyhow!("not testnet"),
        ));
    }
    let tx = Transaction {
        kind: TxKind::Faucet,
        inputs: vec![],
        outputs: vec![CoinData {
            covhash: wallet.read().my_covenant().hash(),
            value: 1001 * MICRO_CONVERTER,
            denom: Denom::Mel,
            additional_data: vec![],
        }],
        data: (0..32)
            .map(|_| nanorand::tls_rng().generate_range(u8::MIN, u8::MAX))
            .collect(),
        fee: MICRO_CONVERTER,
        scripts: vec![],
        sigs: vec![],
    };
    // we mark the TX as sent in this thread
    let txhash = tx.hash_nosigs();
    wallet.write().commit_sent(tx).map_err(to_badreq)?;
    Ok(Body::from_json(&txhash)?)
}

fn to_badreq<E: Into<anyhow::Error> + Send + 'static + Sync + Debug>(e: E) -> tide::Error {
    tide::Error::new(StatusCode::BadRequest, e)
}

fn to_forbidden<E: Into<anyhow::Error> + Send + 'static + Sync + Debug>(e: E) -> tide::Error {
    tide::Error::new(StatusCode::Forbidden, e)
}

fn to_notfound<E: Into<anyhow::Error> + Send + 'static + Sync + Debug>(e: E) -> tide::Error {
    tide::Error::new(StatusCode::NotFound, e)
}

fn to_badgateway<E: Into<anyhow::Error> + Send + 'static + Sync + Debug>(e: E) -> tide::Error {
    log::warn!("bad upstream: {:#?}", e);
    tide::Error::new(StatusCode::BadGateway, e)
}

fn notfound() -> tide::Error {
    tide::Error::new(StatusCode::NotFound, anyhow::anyhow!("not found"))
}
