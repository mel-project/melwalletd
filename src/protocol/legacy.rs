use melwalletd_prot::types::{PrepareTxArgs, WalletAccessError};
use melwalletd_prot::MelwalletdProtocol;
use tide::{Request, Server};

use crate::state::AppState;

use anyhow::Context;
use http_types::{convert::Deserialize, Body, StatusCode};
use melstructs::{Denom, PoolKey, Transaction};
use std::fmt::Debug;
use tmelcrypt::HashVal;

fn to_badreq<E: Into<anyhow::Error> + Send + 'static + Sync + Debug>(e: E) -> tide::Error {
    tide::Error::new(StatusCode::BadRequest, e)
}

fn from_wallet_access(e: WalletAccessError) -> tide::Error {
    match e {
        WalletAccessError::NotFound => tide::Error::new(StatusCode::NotFound, e),
        WalletAccessError::Locked => tide::Error::new(StatusCode::Forbidden, e),
        WalletAccessError::Other(e) => {
            tide::Error::new(StatusCode::InternalServerError, anyhow::anyhow!(e))
        }
    }
}

pub async fn summarize_wallet(req: Request<AppState>) -> tide::Result<Body> {
    let wallet_name = req.param("name")?;
    let state = req.state();
    let wallet_summary = state
        .wallet_summary(wallet_name.to_owned())
        .await
        .map_err(from_wallet_access)?;
    Body::from_json(&wallet_summary)
}

pub async fn get_summary(req: Request<AppState>) -> tide::Result<Body> {
    Body::from_json(&req.state().latest_header().await?)
}

pub async fn get_pool(req: Request<AppState>) -> tide::Result<Body> {
    let pool_key: PoolKey = req
        .param("pair")?
        .replace(':', "/")
        .parse()
        .map_err(to_badreq)?;

    Body::from_json(&req.state().melswap_info(pool_key).await?)
}

pub async fn get_pool_info(req: Request<AppState>) -> tide::Result<Body> {
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
    Body::from_json(&req.state().simulate_swap(to, from, value).await?)
}

pub async fn list_wallets(req: Request<AppState>) -> tide::Result<Body> {
    Body::from_json(&req.state().list_wallets().await)
}

pub async fn create_wallet(mut req: Request<AppState>) -> tide::Result<Body> {
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
            .create_wallet(
                wallet_name,
                query.password.unwrap_or_default(),
                query.secret,
            )
            .await?,
    )
}

pub async fn dump_coins(req: Request<AppState>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let rpc = req.state();
    let coins = rpc.dump_coins(wallet_name).await?;
    Body::from_json(&coins)
}

pub async fn dump_transactions(req: Request<AppState>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let rpc = req.state();
    let tx_info = rpc.dump_transactions(wallet_name).await?;
    Body::from_json(&tx_info)
}

pub async fn lock_wallet(req: Request<AppState>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let rpc = req.state();
    rpc.lock_wallet(wallet_name).await?;
    Ok("".into())
}

pub async fn unlock_wallet(mut req: Request<AppState>) -> tide::Result<Body> {
    #[derive(Deserialize)]
    struct Req {
        password: Option<String>,
    }
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let request: Req = req.body_json().await?;
    // attempt to unlock
    let rpc = req.state();
    rpc.unlock_wallet(wallet_name, request.password.unwrap_or_default())
        .await?;
    Ok("".into())
}

pub async fn export_sk_from_wallet(mut req: Request<AppState>) -> tide::Result<Body> {
    #[derive(Deserialize)]
    struct Req {
        password: String,
    }
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let request: Req = req.body_json().await?;
    let rpc = req.state();

    // attempt to unlock
    let sk = rpc.export_sk(wallet_name, request.password).await?;

    Body::from_json(&sk)
}

pub async fn prepare_tx(mut req: Request<AppState>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let request: PrepareTxArgs = req.body_json().await?;
    // calculate fees
    let rpc = req.state();
    let tx = rpc.prepare_tx(wallet_name, request).await?;
    Body::from_json(&tx)
}

pub async fn send_tx(mut req: Request<AppState>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let tx: Transaction = req.body_json().await?;
    let rpc = req.state();
    let tx_hash = rpc.send_tx(wallet_name, tx).await?;
    Body::from_json(&tx_hash)
}

// pub async fn force_revert_tx<T:Melwallet + Send + Sync,State>(mut req: Request<Arc<MelwalletdRpcImpl>>) ->tide::Result<Body> {
//     todo!()
// }

pub async fn get_tx_balance(req: Request<AppState>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let txhash: HashVal = req.param("txhash")?.parse().map_err(to_badreq)?;

    let rpc = req.state();
    let tx_balance = rpc.tx_balance(wallet_name, txhash).await?;
    Body::from_json(&tx_balance)
}

pub async fn get_tx(req: Request<AppState>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let txhash: HashVal = req.param("txhash")?.parse().map_err(to_badreq)?;
    let rpc = req.state();
    let tx = rpc
        .tx_status(wallet_name, txhash)
        .await?
        .context("no such tx")?;
    Body::from_json(&tx)
}

pub async fn send_faucet(req: Request<AppState>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let rpc = req.state();
    let txhash = rpc.send_faucet(wallet_name).await?;
    Body::from_json(&txhash)
}

// pub async fn prepare_stake_tx<T:Melwallet + Send + Sync,State>(mut req: Request<Arc<MelwalletdRpcImpl>>) ->tide::Result<Body> {
//     todo!()
// }

pub fn route_legacy(app: &mut Server<AppState>) {
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
}
