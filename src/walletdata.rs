use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use themelio_stf::melvm::Covenant;
use themelio_structs::{
    BlockHeight, CoinData, CoinDataHeight, CoinID, NetID, StakeDoc, Transaction, TxHash,
};

/// Cloneable in-memory data that can be persisted.
/// Does not store secrets!
#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LegacyWalletData {
    #[serde_as(as = "Vec<(_, _)>")]
    pub unspent_coins: BTreeMap<CoinID, CoinDataHeight>,
    #[serde_as(as = "Vec<(_, _)>")]
    pub spent_coins: BTreeMap<CoinID, CoinDataHeight>,
    #[serde(rename = "tx_in_progress_v2", default)]
    pub tx_in_progress: BTreeMap<TxHash, (Transaction, BlockHeight)>,
    pub tx_confirmed: BTreeMap<TxHash, (Transaction, BlockHeight)>,
    #[serde(default)] 
    pub stake_list: BTreeMap<TxHash, StakeDoc>,
    pub my_covenant: Covenant,
    pub network: NetID,
}

impl LegacyWalletData {
    /// Obtain a reference to network
    pub fn network(&self) -> NetID {
        self.network
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TransactionStatus {
    pub raw: Transaction,
    pub confirmed_height: Option<BlockHeight>,
    pub outputs: Vec<AnnCoinID>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AnnCoinID {
    pub coin_data: CoinData,
    pub is_change: bool,
    pub coin_id: String,
}
