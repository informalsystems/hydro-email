// SPDX-License-Identifier: GPL-3.0-or-later
// Derived from Lay3rLabs/cw-middleware v0.2.0-alpha.15, Layer Labs (Cayman).
// See ../../stake-registry/UPSTREAM.md for provenance and local changes.

use cosmwasm_schema::{cw_serde, QueryResponses};
#[allow(unused_imports)]
use cosmwasm_std::Addr;
use cosmwasm_std::{Binary, Uint256, WasmMsg};
use layer_climb_address::EvmAddr;
use wavs_types::contracts::cosmwasm::service_handler::{WavsEnvelope, WavsSignatureData};

#[cw_serde]
pub struct InstantiateMsg {
    /// Committee or dedicated authority managing operator keys and weights.
    pub owner: String,
    pub service_manager_instantiate: WasmMsg,
}

#[cw_serde]
#[schemaifier(mute_warnings)]
pub enum ExecuteMsg {
    /// Immediately transfer ownership. Only the current owner may call.
    TransferOwnership { new_owner: String },
    /// Set operator details (owner only)
    SetOperatorDetails {
        operator: EvmAddr,
        signing_key: EvmAddr,
        weight: Uint256,
    },
    /// Batch set multiple operator details (owner only)
    BatchSetOperatorDetails {
        operators: Vec<EvmAddr>,
        signing_keys: Vec<EvmAddr>,
        weights: Vec<Uint256>,
    },
}

#[cw_serde]
#[derive(QueryResponses)]
#[schemaifier(mute_warnings)]
pub enum QueryMsg {
    /// Current operator-management authority.
    #[returns(Addr)]
    Owner {},
    /// Check if a signature is valid (equivalent to isValidSignature)
    /// Returns ValidationResult with total voting power and voting power that signed
    #[returns(ValidationResult)]
    ValidateSignature {
        envelope: WavsEnvelope,
        signature_data: WavsSignatureData,
    },
    /// Get operator weight
    #[returns(Uint256)]
    GetOperatorWeight { operator: EvmAddr },
    /// Get operator signing key
    #[returns(Option<EvmAddr>)]
    GetOperatorSigningKey { operator: EvmAddr },
    /// Get latest operator for signing key
    #[returns(Option<EvmAddr>)]
    GetLatestOperatorForSigningKey { signing_key: EvmAddr },
    /// Get service manager address
    #[returns(Addr)]
    GetServiceManager {},
    /// Get total weight
    #[returns(Uint256)]
    GetTotalWeight {},
}

#[cw_serde]
pub struct ValidationResult {
    pub total_voting_power: Uint256,
    pub voting_power_signed: Uint256,
    pub reference_block: u32,
}

#[cw_serde]
pub struct SignatureData {
    pub operators: Vec<EvmAddr>,
    pub signatures: Vec<Binary>,
    pub reference_block: u32,
}

#[cw_serde]
pub struct OperatorDetails {
    pub operator: EvmAddr,
    pub signing_key: EvmAddr,
    pub weight: Uint256,
    pub registered: bool,
}

// Events to match Solidity contract
#[cw_serde]
pub struct OperatorWeightUpdatedEvent {
    pub operator: EvmAddr,
    pub old_weight: Uint256,
    pub new_weight: Uint256,
}

impl OperatorWeightUpdatedEvent {
    pub const EVENT_TYPE: &'static str = "operator-weight-updated";
    pub const EVENT_ATTR_KEY_OPERATOR: &'static str = "operator";
    pub const EVENT_ATTR_KEY_OLD_WEIGHT: &'static str = "old-weight";
    pub const EVENT_ATTR_KEY_NEW_WEIGHT: &'static str = "new-weight";
}

impl From<OperatorWeightUpdatedEvent> for cosmwasm_std::Event {
    fn from(src: OperatorWeightUpdatedEvent) -> Self {
        cosmwasm_std::Event::new(OperatorWeightUpdatedEvent::EVENT_TYPE)
            .add_attribute(
                OperatorWeightUpdatedEvent::EVENT_ATTR_KEY_OPERATOR,
                src.operator.to_string(),
            )
            .add_attribute(
                OperatorWeightUpdatedEvent::EVENT_ATTR_KEY_OLD_WEIGHT,
                src.old_weight.to_string(),
            )
            .add_attribute(
                OperatorWeightUpdatedEvent::EVENT_ATTR_KEY_NEW_WEIGHT,
                src.new_weight.to_string(),
            )
    }
}

#[cw_serde]
pub struct TotalWeightUpdatedEvent {
    pub old_total_weight: Uint256,
    pub new_total_weight: Uint256,
}

impl TotalWeightUpdatedEvent {
    pub const EVENT_TYPE: &'static str = "total-weight-updated";
    pub const EVENT_ATTR_KEY_OLD_TOTAL_WEIGHT: &'static str = "old-total-weight";
    pub const EVENT_ATTR_KEY_NEW_TOTAL_WEIGHT: &'static str = "new-total-weight";
}

impl From<TotalWeightUpdatedEvent> for cosmwasm_std::Event {
    fn from(src: TotalWeightUpdatedEvent) -> Self {
        cosmwasm_std::Event::new(TotalWeightUpdatedEvent::EVENT_TYPE)
            .add_attribute(
                TotalWeightUpdatedEvent::EVENT_ATTR_KEY_OLD_TOTAL_WEIGHT,
                src.old_total_weight.to_string(),
            )
            .add_attribute(
                TotalWeightUpdatedEvent::EVENT_ATTR_KEY_NEW_TOTAL_WEIGHT,
                src.new_total_weight.to_string(),
            )
    }
}

#[cw_serde]
pub struct SigningKeyUpdateEvent {
    pub operator: EvmAddr,
    pub block_number: u64,
    pub new_signing_key: EvmAddr,
    pub old_signing_key: Option<EvmAddr>,
}

impl SigningKeyUpdateEvent {
    pub const EVENT_TYPE: &'static str = "signing-key-update";
    pub const EVENT_ATTR_KEY_OPERATOR: &'static str = "operator";
    pub const EVENT_ATTR_KEY_BLOCK_NUMBER: &'static str = "block-number";
    pub const EVENT_ATTR_KEY_NEW_SIGNING_KEY: &'static str = "new-signing-key";
    pub const EVENT_ATTR_KEY_OLD_SIGNING_KEY: &'static str = "old-signing-key";
}

impl From<SigningKeyUpdateEvent> for cosmwasm_std::Event {
    fn from(src: SigningKeyUpdateEvent) -> Self {
        cosmwasm_std::Event::new(SigningKeyUpdateEvent::EVENT_TYPE)
            .add_attribute(
                SigningKeyUpdateEvent::EVENT_ATTR_KEY_OPERATOR,
                src.operator.to_string(),
            )
            .add_attribute(
                SigningKeyUpdateEvent::EVENT_ATTR_KEY_BLOCK_NUMBER,
                src.block_number.to_string(),
            )
            .add_attribute(
                SigningKeyUpdateEvent::EVENT_ATTR_KEY_NEW_SIGNING_KEY,
                src.new_signing_key.to_string(),
            )
            .add_attribute(
                SigningKeyUpdateEvent::EVENT_ATTR_KEY_OLD_SIGNING_KEY,
                src.old_signing_key
                    .map(|k| k.to_string())
                    .unwrap_or_else(|| "0x0000000000000000000000000000000000000000".to_string()),
            )
    }
}
