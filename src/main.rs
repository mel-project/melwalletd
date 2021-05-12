mod acidjson;
mod multiwallet;
mod walletdata;
use std::path::PathBuf;

use anyhow::Context;
use multiwallet::MultiWallet;
use structopt::StructOpt;
#[derive(StructOpt)]
struct Args {
    #[structopt(long)]
    wallet_dir: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::from_args();
    std::fs::create_dir_all(&args.wallet_dir).context("cannot create wallet_dir")?;
    let multiwallet = MultiWallet::open(&args.wallet_dir)?;
    eprintln!(
        "wallet contents: {:?}",
        multiwallet.list().collect::<Vec<_>>()
    );
    Ok(())
}
