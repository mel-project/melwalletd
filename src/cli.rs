use std::{path::PathBuf, net::SocketAddr, str::FromStr};

use anyhow::Context;
use clap::Parser;
use serde::*;
use themelio_structs::NetID;

#[derive(Parser, Clone, Deserialize, Debug)]
struct Args {
    #[clap(long)]
    /// Required: directory of the wallet database
    wallet_dir: PathBuf,

    #[clap(long)]
    /// melwalletd server address [default: 127.0.0.1:11773]
    listen: Option<SocketAddr>,


    #[clap(long, short, default_value = "*")]
    /// CORS origins allowed to access daemon
    allowed_origins: Option<Vec<String>>, // TODO: validate as urls

    #[clap(long)]
    network_addr: Option<SocketAddr>,

    #[clap(long)]
    netid: Option<NetID>, // TODO: make this NETID

    #[serde(skip_serializing)]
    #[clap(long)]
    config: Option<String>,

    #[serde(skip_serializing)]
    #[clap(long)]
    output_config: bool,

    #[serde(skip_serializing)]
    #[clap(long)]
    dry_run: bool,
}



#[derive(Deserialize, Debug, Serialize)]
struct Config {
    wallet_dir: PathBuf,
    listen: SocketAddr,
    network_addr: SocketAddr,
    allowed_origins: Vec<String>,
    network: NetID,
}
impl Config {
    fn new(
        wallet_dir: Option<PathBuf>,
        listen: Option<SocketAddr>,
        allowed_origins: Option<Vec<String>>,
        network_addr: Option<SocketAddr>,
        network: Option<NetID>,
    ) -> Config {
        let network = network.unwrap_or(NetID::Mainnet);
        let network_addr = network_addr
            .or(first_bootstrap_route(network))
            .expect(&format!(
                "No bootstrap nodes available for network: {network:?}"
            ));
        Config {
            wallet_dir: wallet_dir.expect("Must provide arg: `wallet-dir`"),
            listen: listen.unwrap_or(SocketAddr::from_str("127.0.0.1:11773").unwrap()),
            network_addr,
            allowed_origins: allowed_origins.unwrap_or(vec!["*".into()]),
            network,
        }
    }
}

impl From<Args> for Config {
    fn from(args: Args) -> Self {
        let config = Config::new(
            Some(args.wallet_dir),
            args.listen,
            args.allowed_origins,
            args.network_addr,
            args.netid,
        );
        config
    }
}

impl From<(Args, Args)> for Config {
    fn from(args: (Args, Args)) -> Self {
        let (preference, baseline) = args;
        let config = Config::new(
            Some(preference.wallet_dir).or(Some(baseline.wallet_dir)),
            preference.listen.or(baseline.listen),
            preference.allowed_origins.or(baseline.allowed_origins),
            preference.network_addr.or(baseline.network_addr),
            preference.netid.or(baseline.netid),
        );

        config
    }
}


fn first_bootstrap_route(network: NetID) -> Option<SocketAddr>{
    let routes = themelio_bootstrap::bootstrap_routes(network);
    if routes.is_empty(){ None }
    else {
        Some(routes[0])
    }

}