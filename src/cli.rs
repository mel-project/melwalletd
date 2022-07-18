use std::{path::PathBuf, net::SocketAddr, str::FromStr, convert::TryFrom};

use anyhow::Context;
use clap::Parser;
use serde::*;
use themelio_structs::NetID;

#[derive(Parser, Clone, Deserialize, Debug)]
struct Args {
    #[clap(long)]
    /// Required: directory of the wallet database
    wallet_dir: PathBuf,

    #[clap(long, default_value="127.0.0.1:11773")]
    /// melwalletd server address [default: 127.0.0.1:11773]
    listen: SocketAddr,


    #[clap(long, short, default_value = "*")]
    /// CORS origins allowed to access daemon
    allowed_origins: Vec<String>, // TODO: validate as urls

    #[clap(long)]
    network_addr: Option<SocketAddr>,

    #[clap(long, default_value = "mainnet")]
    netid: NetID, // TODO: make this NETID

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
    // Create's a new config and attempts to discover a reasonable bootstrap node if possible
    fn new(
        wallet_dir: PathBuf,
        listen: SocketAddr,
        allowed_origins: Vec<String>,
        network_addr: SocketAddr,
        network: NetID,
    ) -> Config {
        
        Config {
            wallet_dir,
            listen,
            network_addr,
            allowed_origins,
            network,
        }
    }
}

impl TryFrom<Args> for Config {
    type Error = anyhow::Error;

    fn try_from(args: Args) -> Result<Self, Self::Error> {
        let network_addr = args.network_addr
        .or(first_bootstrap_route(args.netid))
        .context(&format!(
            "No bootstrap nodes available for network: {:?}", args.netid
        ))?;

    let config = Config::new(
        args.wallet_dir,
        args.listen,
        args.allowed_origins,
        network_addr,
        args.netid,
    );
        Ok(config)
    }
}


fn first_bootstrap_route(network: NetID) -> Option<SocketAddr>{
    let routes = themelio_bootstrap::bootstrap_routes(network);
    if routes.is_empty(){ None }
    else {
        Some(routes[0])
    }

}