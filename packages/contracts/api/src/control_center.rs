//! Local copy of hydro control-center messages.
//! Kept local to avoid coupling the API to a contract crate and its CosmWasm version.

use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Decimal, Uint128};

#[cw_serde]
pub struct InstantiateMsg {
    pub deposit_cap: Uint128,
    pub whitelist: Vec<String>,
    pub subvaults: Vec<String>,
    /// None preserves the existing flow with performance fees disabled.
    pub fee_config: Option<FeeConfigInit>,
}

#[cw_serde]
pub struct FeeConfigInit {
    pub fee_rate: Decimal,
    pub fee_recipient: Option<String>,
}
