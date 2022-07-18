use std::{path::PathBuf, net::SocketAddr, str::FromStr, convert::TryFrom, fs::File, io::Read, env};

use anyhow::Context;
use clap::{Parser, ArgGroup, FromArgMatches, ArgMatches};
use serde::*;
use themelio_structs::NetID;

#[derive(Parser, Clone, Deserialize, Debug)]

pub struct Args {
    #[clap(long, group="standard")]
    /// Required: directory of the wallet database
    pub wallet_dir: Option<PathBuf>,

    #[clap(long, default_value="127.0.0.1:11773")]
    #[serde(deserialize_with="default_socket")]
    /// melwalletd server address
    pub listen: SocketAddr,


    #[clap(long, short, default_value = "*")]
    /// CORS origins allowed to access daemon
    pub allowed_origin: Vec<String>, // TODO: validate as urls

    #[clap(long)]
    pub network_addr: Option<SocketAddr>,

    #[clap(long)]
    pub network: Option<NetID>, // TODO: make this NETID

    #[serde(skip_serializing)]
    #[clap(long, group="standard")]
    pub config: Option<String>,

    #[serde(skip_serializing)]
    #[clap(long)]
    pub output_config: bool,

    #[serde(skip_serializing)]
    #[clap(long)]
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

    fn try_from(args: Args) -> Result<Self, Self::Error> {
 
        let config_from_file: Option<&mut Args> = match args.config{
            Some(filename) => {
                let mut config_file = File::open(filename)?;
                let mut buf: String = "".into();
                config_file.read_to_string(&mut buf)?;
                let args: Args = serde_yaml::from_str(&buf)?;
            
                Some(&mut args)
            },
            None => None
        };


        let preference = args;

        let structured_args = match config_from_file {
            Some(baseline) => {
                let wallet_dir = args.wallet_dir.or(baseline.wallet_dir).context("Must include `wallet_dir` in config")?;
                let listen = prefer_flag("--listen", args.listen, baseline.listen);
                let network = args.network.or(baseline.network).unwrap_or(NetID::Mainnet);
                let network_addr = args.network_addr
                    .or(first_bootstrap_route(network))
                    .expect(&format!(
                        "No bootstrap nodes available for network: {network:?}"
                    ));
                let allowed_origins = prefer_flag("--allowed-origin", args.allowed_origin, baseline.allowed_origin);
                Args {
                    wallet_dir,
                    listen,
                    network,
                    network_addr,
                    
                }
        
            },
            None => preference,
        }
        let network = args.network.unwrap_or(NetID::Mainnet);
        let network_addr = args.network_addr
            .or(first_bootstrap_route(network))
            .expect(&format!(
                "No bootstrap nodes available for network: {network:?}"
            ));
    }
}


fn first_bootstrap_route(network: NetID) -> Option<SocketAddr>{
    let routes = themelio_bootstrap::bootstrap_routes(network);
    if routes.is_empty(){ None }
    else {
        Some(routes[0])
    }

}

fn is_flag(s: &str) -> bool{
        let mut flag = false;
        for argument in env::args() {
            if argument == s {
                flag = true;
                break;
            }
        }
        flag
    }


fn prefer_flag<T>(flag: &str, preference: T, baseline: T) -> T{
        if is_flag(flag) {
            preference
        }
        else {
            baseline // config needs listen. this breaks the idea of config defaults
        }
}