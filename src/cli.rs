use std::{path::PathBuf, net::SocketAddr, convert::TryFrom, fs::File, io::Read};

use clap::{Parser, ArgGroup};
use serde::*;
use themelio_structs::NetID;

#[derive(Parser, Clone, Deserialize, Debug)]
#[clap(group(
    ArgGroup::new("options")
        .required(true)
        .args(&["wallet-dir", "config"]),
))]
pub struct Args {
    #[clap(long)]
    /// Required: directory of the wallet database
    pub wallet_dir: Option<PathBuf>,

    #[clap(long, default_value="127.0.0.1:11773")]
    /// melwalletd server address
    pub listen: SocketAddr,


    #[clap(long, short, default_value = "*")]
    /// CORS origins allowed to access daemon
    pub allowed_origin: Vec<String>, // TODO: validate as urls

    #[clap(long)]
    pub network_addr: Option<SocketAddr>,

    #[clap(long, default_value="mainnet")]
    pub network: NetID,

    #[serde(skip_serializing)]
    #[clap(long)]
    pub config: Option<String>,

    #[serde(skip_serializing)]
    #[clap(long)]
    /// send the generated config to stdout
    pub output_config: bool,

    #[serde(skip_serializing)]
    #[clap(long)]
    /// run without starting server
    pub dry_run: bool,
}



#[derive(Deserialize, Debug, Serialize)]
pub struct Config {
    pub wallet_dir: PathBuf,
    pub listen: SocketAddr,
    pub network_addr: SocketAddr,
    pub allowed_origins: Vec<String>,
    pub network: NetID,
}
impl Config {
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

    fn try_from(cmd: Args) -> Result<Self, Self::Error> {

        let config = match cmd.config{
            Some(filename) => {
                let mut config_file = File::open(filename)?;
                let mut buf: String = "".into();
                config_file.read_to_string(&mut buf)?;
                let config: Config = serde_yaml::from_str(&buf)?;

                anyhow::Ok(config)
            },
            None => {
                let args = cmd;
                let network = args.network;
                let network_addr = args.network_addr
                    .or(first_bootstrap_route(network))
                    .expect(&format!(
                        "No bootstrap nodes available for network: {network:?}"
                    ));
                Ok(Config::new(
                    args.wallet_dir.unwrap(),
                    args.listen,
                    args.allowed_origin,
                    network_addr,
                    network,
                ))
            }
        };
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
