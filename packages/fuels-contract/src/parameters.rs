use fuel_tx::AssetId;
use fuel_vm::consts::REG_CGAS;
use fuels_core::constants::{
    DEFAULT_BYTE_PRICE, DEFAULT_GAS_LIMIT, DEFAULT_GAS_PRICE, DEFAULT_MATURITY, NATIVE_ASSET_ID,
};

const NO_GAS_FORWARDED: Option<u64> = None;

#[derive(Debug)]
pub struct TxParameters {
    pub gas_price: u64,
    pub gas_limit: u64,
    pub byte_price: u64,
    pub maturity: u32,
}

#[derive(Debug)]
pub struct CallParameters {
    pub gas_to_forward: Option<u64>,
    pub amount: u64,
    pub asset_id: AssetId,
}

impl CallParameters {
    pub fn new(gas_to_forward: Option<u64>, amount: Option<u64>, asset_id: Option<AssetId>) ->
                                                                                            Self {
        Self {
            gas_to_forward:  gas_to_forward,
            amount: amount.unwrap_or(0),
            asset_id: asset_id.unwrap_or(NATIVE_ASSET_ID),
        }
    }
}

impl Default for CallParameters {
    fn default() -> Self {
        Self {
            gas_to_forward: Option::None,
            amount: 0,
            asset_id: NATIVE_ASSET_ID,
        }
    }
}

impl Default for TxParameters {
    fn default() -> Self {
        Self {
            gas_price: DEFAULT_GAS_PRICE,
            gas_limit: DEFAULT_GAS_LIMIT,
            byte_price: DEFAULT_BYTE_PRICE,
            // By default, transaction is immediately valid
            maturity: DEFAULT_MATURITY,
        }
    }
}

impl TxParameters {
    pub fn new(
        gas_price: Option<u64>,
        gas_limit: Option<u64>,
        byte_price: Option<u64>,
        maturity: Option<u32>,
    ) -> Self {
        Self {
            gas_price: gas_price.unwrap_or(DEFAULT_GAS_PRICE),
            gas_limit: gas_limit.unwrap_or(DEFAULT_GAS_LIMIT),
            byte_price: byte_price.unwrap_or(DEFAULT_BYTE_PRICE),
            maturity: maturity.unwrap_or(DEFAULT_MATURITY),
        }
    }
}
