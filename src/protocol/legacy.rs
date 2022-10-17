use std::marker::PhantomData;
use std::sync::Arc;

use http_types::headers::HeaderValue;
use melwalletd_prot::protocol::MelwalletdProtocol;
use melwalletd_prot::types::MelwalletdHelpers;
use melwalletd_prot::types::PrepareTxArgs;
use tide::{security::CorsMiddleware, Request, Server};

use crate::cli::Config;

use anyhow::anyhow;
use anyhow::Context;
use http_types::{convert::Deserialize, Body, StatusCode};
use std::fmt::Debug;
use themelio_structs::{Denom, PoolKey, Transaction};
use tmelcrypt::HashVal;


use super::rpc::MelwalletdRpcImpl;

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

async fn log_request<T>(req: Request<T>) -> Request<T> {
    log::info!("{}", req.url());
    req
}

fn to_badreq<E: Into<anyhow::Error> + Send + 'static + Sync + Debug>(e: E) -> tide::Error {
    tide::Error::new(StatusCode::BadRequest, e)
}

// fn to_forbidden<E: Into<anyhow::Error> + Send + 'static + Sync + Debug>(e: E) -> tide::Error {
//     tide::Error::new(StatusCode::Forbidden, e)
// }

// fn to_notfound<E: Into<anyhow::Error> + Send + 'static + Sync + Debug>(e: E) -> tide::Error {
//     tide::Error::new(StatusCode::NotFound, e)
// }

// fn to_badgateway<E: Into<anyhow::Error> + Send + 'static + Sync + Debug>(e: E) -> tide::Error {
//     log::warn!("bad upstream: {:#?}", e);
//     tide::Error::new(StatusCode::BadGateway, e)
// }

// fn notfound_with(s: String) -> tide::Error {
//     tide::Error::new(StatusCode::NotFound, anyhow::anyhow!("{s}"))
// }

// fn wallet_notfound() -> tide::Error {
//     notfound_with("wallet not found".into())
// }

// pub async fn log_request<T>(req: Request<T>) -> Request<T> {
//     log::info!("{}", req.url());
//     req
// }
struct LegacyHelpers<State: MelwalletdHelpers + Send + Sync> {
    _phantom: PhantomData<State>,
}

impl<State: MelwalletdHelpers + Send + Sync> LegacyHelpers<State> {
    pub async fn summarize_wallet(
        req: Request<Arc<MelwalletdRpcImpl<State>>>,
    ) -> tide::Result<Body> {
        let wallet_name = req.param("name")?;
        let state = req.state();
        let wallet_summary = state.summarize_wallet(wallet_name.to_owned()).await?;
        Body::from_json(&wallet_summary)
    }

    pub async fn get_summary(req: Request<Arc<MelwalletdRpcImpl<State>>>) -> tide::Result<Body> {
        Body::from_json(&req.state().get_summary().await?)
    }

    pub async fn get_pool(req: Request<Arc<MelwalletdRpcImpl<State>>>) -> tide::Result<Body> {
        let pool_key: PoolKey = req
            .param("pair")?
            .replace(':', "/")
            .parse()
            .map_err(to_badreq)?;
        let pool_key = pool_key
            .to_canonical()
            .ok_or_else(|| to_badreq(anyhow!("bad pool key")))?;
        Body::from_json(&req.state().get_pool(pool_key).await?)
    }

    pub async fn get_pool_info(req: Request<Arc<MelwalletdRpcImpl<State>>>) -> tide::Result<Body> {
        #[derive(Deserialize)]
        struct Req {
            from: String,
            to: String,
            value: u128,
        }
        let query: Req = req.query()?;
        let value = query.value;
        let from = Denom::from_bytes(&hex::decode(&query.from)?).context("oh no")?;
        let to = Denom::from_bytes(&hex::decode(&query.to)?).context("oh no")?;
        Body::from_json(&req.state().simulate_pool_swap(to, from, value).await?)
    }

    pub async fn list_wallets(req: Request<Arc<MelwalletdRpcImpl<State>>>) -> tide::Result<Body> {
        Body::from_json(&req.state().state.list_wallets().await)
    }

    pub async fn create_wallet(
        mut req: Request<Arc<MelwalletdRpcImpl<State>>>,
    ) -> tide::Result<Body> {
        #[derive(Deserialize, Debug, Default)]
        struct Query {
            password: Option<String>,
            secret: Option<String>,
        }

        let body = &req.body_string().await?;
        let query: Query = serde_json::from_str(body)?;

        let wallet_name = req.param("name").map(|v| v.to_string())?;
        Body::from_json(
            &req.state()
                .create_wallet(wallet_name, query.password, query.secret)
                .await?,
        )
    }

    pub async fn dump_coins(req: Request<Arc<MelwalletdRpcImpl<State>>>) -> tide::Result<Body> {
        let wallet_name = req.param("name").map(|v| v.to_string())?;
        let rpc = req.state();
        let coins = rpc.dump_coins(wallet_name).await?;
        Body::from_json(&coins)
    }

    pub async fn dump_transactions(
        req: Request<Arc<MelwalletdRpcImpl<State>>>,
    ) -> tide::Result<Body> {
        let wallet_name = req.param("name").map(|v| v.to_string())?;
        let rpc = req.state();
        let tx_info = rpc.dump_transactions(wallet_name).await?;
        Body::from_json(&tx_info)
    }

    pub async fn lock_wallet(req: Request<Arc<MelwalletdRpcImpl<State>>>) -> tide::Result<Body> {
        let wallet_name = req.param("name").map(|v| v.to_string())?;
        let rpc = req.state();
        rpc.lock_wallet(wallet_name).await;
        Ok("".into())
    }

    pub async fn unlock_wallet(
        mut req: Request<Arc<MelwalletdRpcImpl<State>>>,
    ) -> tide::Result<Body> {
        #[derive(Deserialize)]
        struct Req {
            password: Option<String>,
        }
        let wallet_name = req.param("name").map(|v| v.to_string())?;
        let request: Req = req.body_json().await?;
        // attempt to unlock
        let rpc = req.state();
        rpc.unlock_wallet(wallet_name, request.password).await?;
        Ok("".into())
    }

    pub async fn export_sk_from_wallet(
        mut req: Request<Arc<MelwalletdRpcImpl<State>>>,
    ) -> tide::Result<Body> {
        #[derive(Deserialize)]
        struct Req {
            password: Option<String>,
        }
        let wallet_name = req.param("name").map(|v| v.to_string())?;
        let request: Req = req.body_json().await?;
        let rpc = req.state();

        // attempt to unlock
        let sk = rpc
            .export_sk_from_wallet(wallet_name, request.password)
            .await?;

        Body::from_json(&sk)
    }

    pub async fn prepare_tx(mut req: Request<Arc<MelwalletdRpcImpl<State>>>) -> tide::Result<Body> {
        let wallet_name = req.param("name").map(|v| v.to_string())?;
        let request: PrepareTxArgs = req.body_json().await?;
        // calculate fees
        let rpc = req.state();
        let tx = rpc.prepare_tx(wallet_name, request).await?;
        Body::from_json(&tx)
    }

    pub async fn send_tx(mut req: Request<Arc<MelwalletdRpcImpl<State>>>) -> tide::Result<Body> {

        let wallet_name = req.param("name").map(|v| v.to_string())?;
        let tx: Transaction = req.body_json().await?;
        let rpc = req.state();
        log::info!("Attempting to send transaction");
        let tx_hash = rpc.send_tx(wallet_name, tx).await?;
        log::info!("Sent to send transaction");
        Body::from_json(&tx_hash)
    }

    // pub async fn force_revert_tx<T:Melwallet + Send + Sync,State>(mut req: Request<Arc<MelwalletdRpcImpl<State>>>) ->tide::Result<Body> {
    //     todo!()
    // }

    pub async fn get_tx_balance(req: Request<Arc<MelwalletdRpcImpl<State>>>) -> tide::Result<Body> {
        let wallet_name = req.param("name").map(|v| v.to_string())?;
        let txhash: HashVal = req.param("txhash")?.parse().map_err(to_badreq)?;

        let rpc = req.state();
        let tx_balance = rpc.get_tx_balance(wallet_name, txhash).await?;
        Body::from_json(&tx_balance)
    }

    pub async fn get_tx(req: Request<Arc<MelwalletdRpcImpl<State>>>) -> tide::Result<Body> {
        let wallet_name = req.param("name").map(|v| v.to_string())?;
        let txhash: HashVal = req.param("txhash")?.parse().map_err(to_badreq)?;
        let rpc = req.state();
        let tx = rpc.get_tx(wallet_name, txhash).await?;
        Body::from_json(&tx)
    }

    pub async fn send_faucet(req: Request<Arc<MelwalletdRpcImpl<State>>>) -> tide::Result<Body> {
        let wallet_name = req.param("name").map(|v| v.to_string())?;
        let rpc = req.state();
        let txhash = rpc.send_faucet(wallet_name).await?;
        Body::from_json(&txhash)
    }
}

// pub async fn prepare_stake_tx<T:Melwallet + Send + Sync,State>(mut req: Request<Arc<MelwalletdRpcImpl<State>>>) ->tide::Result<Body> {
//     todo!()
// }

pub async fn init_server<T: Send + Sync + 'static>(
    config: Arc<Config>,
    state: T,
) -> anyhow::Result<Server<Arc<T>>> {
    let state = Arc::new(state);
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

pub fn legacy_server<State: MelwalletdHelpers + Send + Sync + 'static>(
    mut app: Server<Arc<MelwalletdRpcImpl<State>>>,
) -> anyhow::Result<Server<Arc<MelwalletdRpcImpl<State>>>> {
    app.at("/summary").get(LegacyHelpers::get_summary);
    app.at("/pools/:pair").get(LegacyHelpers::get_pool);
    app.at("/pool_info").post(LegacyHelpers::get_pool_info);
    app.at("/wallets").get(LegacyHelpers::list_wallets);
    app.at("/wallets/:name")
        .get(LegacyHelpers::summarize_wallet);
    app.at("/wallets/:name").put(LegacyHelpers::create_wallet);
    app.at("/wallets/:name/lock")
        .post(LegacyHelpers::lock_wallet);
    app.at("/wallets/:name/unlock")
        .post(LegacyHelpers::unlock_wallet);
    app.at("/wallets/:name/export-sk")
        .post(LegacyHelpers::export_sk_from_wallet);
    app.at("/wallets/:name/coins")
        .get(LegacyHelpers::dump_coins);
    app.at("/wallets/:name/prepare-tx")
        .post(LegacyHelpers::prepare_tx);
    app.at("/wallets/:name/send-tx")
        .post(LegacyHelpers::send_tx);
    app.at("/wallets/:name/send-faucet")
        .post(LegacyHelpers::send_faucet);
    app.at("/wallets/:name/transactions")
        .get(LegacyHelpers::dump_transactions);
    app.at("/wallets/:name/transactions/:txhash")
        .get(LegacyHelpers::get_tx);
    app.at("/wallets/:name/transactions/:txhash/balance")
        .get(LegacyHelpers::get_tx_balance);
    Ok(app)
}

