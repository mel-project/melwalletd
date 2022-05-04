use acidjson::AcidJson;
use anyhow::Context;
use dashmap::DashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::walletdata::LegacyWalletData;

/// Represents a whole directory of wallet JSON files
#[derive(Clone)]
pub struct LegacyMultiWallet {
    wallet_cache: Arc<DashMap<String, AcidJson<LegacyWalletData>>>,
    dirname: PathBuf,
}

fn valid_wallet_name(name: &str) -> bool {
    name.chars().all(|x| x.is_ascii_alphanumeric() || x == '_')
}

impl LegacyMultiWallet {
    /// Opens a new MultiWallet.
    pub fn open(directory: &Path) -> anyhow::Result<Self> {
        std::fs::read_dir(directory).context("cannot open directory")?;
        Ok(LegacyMultiWallet {
            wallet_cache: Default::default(),
            dirname: directory.to_owned(),
        })
    }

    /// Lists all the wallets in the directory.
    pub fn list(&self) -> impl Iterator<Item = String> {
        std::fs::read_dir(&self.dirname)
            .unwrap()
            .map(|v| v.unwrap().file_name().to_string_lossy().to_string())
            .filter(|v| v.ends_with(".json"))
            .map(|v| v.replace(".json", ""))
            .filter(|v| valid_wallet_name(v))
    }

    /// Obtains a wallet by name.
    pub fn get_wallet(&self, name: &str) -> anyhow::Result<AcidJson<LegacyWalletData>> {
        let fname = format!("{}.json", name);
        let mut fpath = self.dirname.clone();
        fpath.push(PathBuf::from(fname));
        let labooyah = self
            .wallet_cache
            .entry(name.to_string())
            .or_try_insert_with(|| AcidJson::open(&fpath))?;
        Ok(labooyah.value().clone())
    }
}
