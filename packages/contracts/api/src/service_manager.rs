// SPDX-License-Identifier: GPL-3.0-or-later
// Derived from Lay3rLabs/cw-middleware v0.2.0-alpha.15, Layer Labs (Cayman).
// See ../../service-manager/UPSTREAM.md for provenance and local changes.

use cosmwasm_schema::{cw_serde, QueryResponses};
use wavs_types::contracts::cosmwasm::service_manager::{
    ServiceManagerExecuteMessages, ServiceManagerQueryMessages,
};

#[cw_serde]
pub struct InstantiateMsg {
    pub admin: String,
}

#[cw_serde]
#[schemaifier(mute_warnings)]
pub enum ExecuteMsg {
    /// Immediately transfer application administration. Only the current admin may call.
    TransferAdmin { new_admin: String },
    #[serde(untagged)]
    Wavs(ServiceManagerExecuteMessages),
}

#[cw_serde]
#[derive(QueryResponses)]
#[schemaifier(mute_warnings)]
pub enum MirrorServiceManagerQueryMessages {
    /// Get the current admin address
    #[returns(cosmwasm_std::Addr)]
    Admin {},
    /// Get the stake registry address
    #[returns(cosmwasm_std::Addr)]
    StakeRegistry {},
}

#[cw_serde]
#[derive(QueryResponses)]
#[query_responses(nested)]
#[schemaifier(mute_warnings)]
pub enum QueryMsg {
    #[serde(untagged)]
    Mirror(MirrorServiceManagerQueryMessages),
    #[serde(untagged)]
    Wavs(ServiceManagerQueryMessages),
}
