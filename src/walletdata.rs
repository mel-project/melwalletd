use serde::{Deserialize, Serialize};

use themelio_structs::{BlockHeight, CoinData, Transaction};

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
