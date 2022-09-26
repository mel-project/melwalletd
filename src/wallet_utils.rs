


use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;
use anyhow::Context;
use serde::*;
use themelio_structs::PoolKey;
use themelio_structs::{
    BlockHeight, CoinData, CoinID, CoinValue, Denom, NetID, Transaction, TxKind,
};
use base32::Alphabet;

use tide::{Body, Request, StatusCode};
use tmelcrypt::{Ed25519SK, HashVal, Hashable};
use crate::state::AppState;
use crate::walletdata::{AnnCoinID, TransactionStatus};

use crate::{signer::Signer};


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

// fn notfound_with(s: String) -> tide::Error {
//     tide::Error::new(StatusCode::NotFound, anyhow::anyhow!("{s}"))
// }

// fn wallet_notfound() -> tide::Error {
//     notfound_with("wallet not found".into())
// }


pub async fn summarize_wallet(req: Request<Arc<AppState>>) -> tide::Result<Body> {

    let wallet_name = req.param("name")?;
    let wallet_list = req.state().list_wallets().await;
    let wallets = wallet_list
        .get(wallet_name)
        .cloned()
        .context("wallet not found")
        .map_err(to_notfound)?;
    Body::from_json(&wallets)

}
pub async fn log_request<T>(req: Request<T>) -> Request<T> {
    log::info!("{}", req.url());
    req
}

pub async fn get_summary(req: Request<Arc<AppState>>) -> tide::Result<Body> {
    let client = req.state().client.clone();
    let snap = client.snapshot().await.map_err(to_badgateway)?;
    Body::from_json(&snap.current_header())
}

pub async fn get_pool(req: Request<Arc<AppState>>) -> tide::Result<Body> {
    let client = req.state().client.clone();
    let pool_key: PoolKey = req
        .param("pair")?
        .replace(':', "/")
        .parse()
        .map_err(to_badreq)?;
    println!("You get a pool key: {}", pool_key);
    let pool_key = pool_key
        .to_canonical()
        .ok_or_else(|| to_badreq(anyhow::anyhow!("bad pool key")))?;
    let pool_state = client
        .snapshot()
        .await
        .map_err(to_badgateway)?
        .get_pool(pool_key)
        .await
        .map_err(to_badgateway)?
        .ok_or_else(|| to_badreq(anyhow::anyhow!("pool not found")))?;
    Body::from_json(&pool_state)
}

pub async fn get_pool_info(mut req: Request<Arc<AppState>>) -> tide::Result<Body> {
    #[derive(Deserialize)]
    struct Req {
        from: String,
        to: String,
        value: u128,
    }
    #[derive(Serialize)]
    struct Resp {
        result: u128,
        price_impact: f64,
        poolkey: String,
    }

    let query: Req = req.body_json().await?;

    let from = Denom::from_bytes(&hex::decode(&query.from)?).context("oh no")?;
    let to = Denom::from_bytes(&hex::decode(&query.to)?).context("oh no")?;

    let client = req.state().client.clone();
    if from == to {
        return Err(to_badreq(anyhow::anyhow!(
            "cannot swap between identical denoms"
        )));
    }
    let pool_key = PoolKey::new(from, to);
    let pool_state = client
        .snapshot()
        .await
        .map_err(to_badgateway)?
        .get_pool(pool_key)
        .await
        .map_err(to_badgateway)?
        .ok_or_else(|| to_badreq(anyhow::anyhow!("pool not found")))?;

    let left_to_right = pool_key.left == from;

    let r = if left_to_right {
        let old_price = pool_state.lefts as f64 / pool_state.rights as f64;
        let mut new_pool_state = pool_state;
        let (_, new) = new_pool_state.swap_many(query.value, 0);
        let new_price = new_pool_state.lefts as f64 / new_pool_state.rights as f64;
        Resp {
            result: new,
            price_impact: (new_price / old_price - 1.0),
            poolkey: hex::encode(pool_key.to_bytes()),
        }
    } else {
        let old_price = pool_state.rights as f64 / pool_state.lefts as f64;
        let mut new_pool_state = pool_state;
        let (new, _) = new_pool_state.swap_many(0, query.value);
        let new_price = new_pool_state.rights as f64 / new_pool_state.lefts as f64;
        Resp {
            result: new,
            price_impact: (new_price / old_price - 1.0),
            poolkey: hex::encode(pool_key.to_bytes()),
        }
    };

    Body::from_json(&r)
}

pub async fn list_wallets(req: Request<Arc<AppState>>) -> tide::Result<Body> {
    Body::from_json(&req.state().list_wallets().await)
}

pub async fn create_wallet(mut req: Request<Arc<AppState>>) -> tide::Result<Body> {
    #[derive(Deserialize, Debug, Default)]
    struct Query {
        password: Option<String>,
        secret: Option<String>,
    }
    
    let body = &req.body_string().await?;
    let query: Query = serde_json::from_str(body)?;

    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let sk = if let Some(secret) = query.secret {
        // We must reconstruct the secret key using the ed25519-dalek library
        let secret =
            base32::decode(Alphabet::Crockford, &secret).context("cannot decode secret key")?;
        let secret = ed25519_dalek::SecretKey::from_bytes(&secret)?;
        let public: ed25519_dalek::PublicKey = (&secret).into();
        let mut vv = [0u8; 64];
        vv[0..32].copy_from_slice(&secret.to_bytes());
        vv[32..].copy_from_slice(&public.to_bytes());
        Ed25519SK(vv)
    } else {
        tmelcrypt::ed25519_keygen().1
    };
    req.state()
        .create_wallet(&wallet_name, sk, query.password)
        .await
        .context("cannot create wallet")?;
    Ok("".into())
}

pub async fn dump_coins(req: Request<Arc<AppState>>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let wallet = req
        .state()
        .get_wallet(&wallet_name)
        .await
        .context("not found")
        .map_err(to_notfound)?;
    let coins = wallet.get_coin_mapping(true, false).await;
    Body::from_json(&coins.into_iter().collect::<Vec<_>>())
}

pub async fn dump_transactions(req: Request<Arc<AppState>>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let wallet = req
        .state()
        .get_wallet(&wallet_name)
        .await
        .context("not found")
        .map_err(to_notfound)?;
    let transactions = wallet.get_transaction_history().await;
    Body::from_json(&transactions)
}

pub async fn lock_wallet(req: Request<Arc<AppState>>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    req.state().lock(&wallet_name);
    Ok("".into())
}

pub async fn unlock_wallet(mut req: Request<Arc<AppState>>) -> tide::Result<Body> {
    #[derive(Deserialize)]
    struct Req {
        password: Option<String>,
    }
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let request: Req = req.body_json().await?;
    // attempt to unlock
    req.state()
        .unlock(&wallet_name, request.password)
        .context("incorrect password")
        .map_err(to_forbidden)?;
    Ok("".into())
}

pub async fn export_sk_from_wallet(mut req: Request<Arc<AppState>>) -> tide::Result<Body> {
    #[derive(Deserialize)]
    struct Req {
        password: Option<String>,
    }
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let request: Req = req.body_json().await?;
    // attempt to unlock
    let secret = req
        .state()
        .get_secret_key(&wallet_name, request.password)
        .context("incorrect password")
        .map_err(to_forbidden)?;
    Ok(base32::encode(Alphabet::Crockford, &secret.0[..32]).into())
}

// pub async fn prepare_stake_tx(req: Request<Arc<AppState>>) -> tide::Result<Body> {
//     todo!()
// }

pub async fn prepare_tx(mut req: Request<Arc<AppState>>) -> tide::Result<Body> {
    #[derive(Deserialize, Debug)]
    struct Req {
        #[serde(default)]
        kind: Option<TxKind>,
        inputs: Vec<CoinID>,
        outputs: Vec<CoinData>,
        #[serde(default, with = "stdcode::hexvec")]
        covenants: Vec<Vec<u8>>,
        data: Option<String>,
        #[serde(default)]
        nobalance: Vec<Denom>,
        #[serde(default)]
        fee_ballast: usize,
        signing_key: Option<String>,
    }
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let request: Req = req.body_json().await?;
    let signing_key: Arc<dyn Signer> = if let Some(signing_key) = request.signing_key.as_ref() {
        Arc::new(signing_key.parse::<Ed25519SK>()?)
    } else {
        req.state()
            .get_signer(&wallet_name)
            .context("wallet is locked")
            .map_err(to_forbidden)?
    };
    let wallet = req
        .state()
        .get_wallet(&wallet_name)
        .await
        .context("no wallet")
        .map_err(to_badreq)?;

    // calculate fees
    let client = req.state().client.clone();
    let snapshot = client.snapshot().await.map_err(to_badgateway)?;
    let fee_multiplier = snapshot.current_header().fee_multiplier;
    let kind = request.kind;
    let data = match request.data.as_ref() {
        Some(v) => Some(hex::decode(v).map_err(to_badreq)?),
        None => None,
    };
    let prepared_tx = wallet
        .prepare(
            request.inputs.clone(),
            request.outputs.clone(),
            fee_multiplier,
            |mut tx: Transaction| {
                if let Some(kind) = kind {
                    tx.kind = kind
                }
                if let Some(data) = data.clone() {
                    tx.data = data
                }
                tx.covenants.extend_from_slice(&request.covenants);
                for i in 0..tx.inputs.len() {
                    tx = signing_key.sign_tx(tx, i)?;
                }
                Ok(tx)
            },
            request.nobalance.clone(),
            request.fee_ballast,
            req.state().client.snapshot().await?,
        )
        .await
        .map_err(to_badreq)?;

    Body::from_json(&prepared_tx)
}

pub async fn send_tx(mut req: Request<Arc<AppState>>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let tx: Result<Transaction,_> = req.body_json().await;
    match tx{
        Ok(_) => println!("good tx"),
        Err(e) => {
            println!("{e:?}");
            return Err(e);
        },
    }
    let tx = tx?;
    let wallet = req
        .state()
        .get_wallet(&wallet_name)
        .await
        .context("fail")
        .map_err(to_badreq)?;
    let snapshot = req.state().client.snapshot().await?;
    // we send it off ourselves
    snapshot.get_raw().send_tx(tx.clone()).await?;
    // we mark the TX as sent in this thread.
    wallet
        .commit_sent(
            tx.clone(),
            snapshot.current_header().height + BlockHeight(10),
        )
        .await
        .map_err(to_badreq)?;
    log::info!("sent transaction with hash {}", tx.hash_nosigs());
    Body::from_json(&tx.hash_nosigs())
}

// pub async fn force_revert_tx(req: Request<Arc<AppState>>) -> tide::Result<Body> {
//     todo!()
// }

pub async fn get_tx_balance(req: Request<Arc<AppState>>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let wallet = req
        .state()
        .get_wallet(&wallet_name)
        .await
        .context("wtf")
        .map_err(to_badreq)?;
    let txhash: HashVal = req.param("txhash")?.parse().map_err(to_badreq)?;
    let raw = wallet
        .get_transaction(txhash.into(), async {
            Ok(req.state().client.snapshot().await?)
        })
        .await
        .map_err(to_badgateway)?
        .context("not found")
        .map_err(to_notfound)?;
    // Is this self-originated? We check the covenants
    let self_originated = raw.covenants.iter().any(|c| c.hash() == wallet.address().0);
    // Total balance out
    let mut balance: BTreeMap<String, i128> = BTreeMap::new();
    // Add all outputs to balance
    if self_originated {
        *balance
            .entry(hex::encode(Denom::Mel.to_bytes()))
            .or_default() -= raw.fee.0 as i128;
    }
    for (idx, output) in raw.outputs.iter().enumerate() {
        let coinid = raw.output_coinid(idx as u8);
        let denom_key = hex::encode(output.denom.to_bytes());
        // first we *deduct* any balance if this self-originated
        if self_originated {
            *balance.entry(denom_key).or_default() -= output.value.0 as i128;
        }
        // then, if we find this value in our coins, we add it back. this turns out to take care of swap tx well
        if let Some(ours) = wallet.get_one_coin(coinid).await {
            let denom_key = hex::encode(ours.denom.to_bytes());
            if ours.covhash == wallet.address() {
                *balance.entry(denom_key).or_default() += ours.value.0 as i128;
            }
        }
    }
    Body::from_json(&(self_originated, raw.kind, balance))
}

pub async fn get_tx(req: Request<Arc<AppState>>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;

    let wallet = req
        .state()
        .get_wallet(&wallet_name)
        .await
        .context("wtf")
        .map_err(to_badreq)?;
    let txhash: HashVal = req.param("txhash")?.parse().map_err(to_badreq)?;

    // Must either be pending or

    let raw = wallet
        .get_cached_transaction(txhash.into())
        .await
        .context("not found")
        .map_err(to_notfound)?;
    let mut confirmed_height = None;
    for idx in 0..raw.outputs.len() {
        if let Some(cdh) = wallet
            .get_coin_confirmation(raw.output_coinid(idx as u8))
            .await
        {
            confirmed_height = Some(cdh.height);
        }
    }
    let outputs = raw
        .outputs
        .iter()
        .enumerate()
        .map(|(i, cd)| {
            let coin_id = raw.output_coinid(i as u8).to_string();
            let is_change = cd.covhash == wallet.address();
            let coin_data = cd.clone();
            AnnCoinID {
                coin_data,
                is_change,
                coin_id,
            }
        })
        .collect();

    if confirmed_height.is_none() {
        // Must be pending
        if !wallet.is_pending(txhash.into()).await {
            Err(anyhow::anyhow!(
                "no longer pending but not confirmed; probably gave up"
            ))
            .map_err(to_notfound)?;
        }
    }
    Body::from_json(&TransactionStatus {
        raw,
        confirmed_height,
        outputs,
    })
}

pub async fn send_faucet(req: Request<Arc<AppState>>) -> tide::Result<Body> {
    let wallet_name = req.param("name").map(|v| v.to_string())?;
    let network = req.state().network;
    let wallet = req
        .state()
        .get_wallet(&wallet_name)
        .await
        .context("wtf")
        .map_err(to_badreq)?;
    if network == NetID::Mainnet {
        return Err(tide::Error::new(
            StatusCode::BadRequest,
            anyhow::anyhow!("faucet is not supported on mainnet"),
        ));
    }
    let tx = Transaction {
        kind: TxKind::Faucet,
        inputs: vec![],
        outputs: vec![CoinData {
            covhash: wallet.address(),
            value: CoinValue::from_millions(1001u64),
            denom: Denom::Mel,
            additional_data: vec![],
        }],
        data: (0..32).map(|_| fastrand::u8(0..=255)).collect(),
        fee: CoinValue::from_millions(1001u64),
        covenants: vec![],
        sigs: vec![],
    };
    println!("{}",::serde_json::to_string_pretty(&tx)?);
    // we mark the TX as sent in this thread
    let txhash = tx.hash_nosigs();
    wallet
        .commit_sent(tx, BlockHeight(10000000000))
        .await
        .map_err(to_badreq)?;
    Body::from_json(&txhash)
}
