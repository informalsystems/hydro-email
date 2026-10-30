// SPDX-License-Identifier: GPL-3.0-or-later
// Derived from Lay3rLabs/cw-middleware v0.2.0-alpha.15, Layer Labs (Cayman).
// See ../UPSTREAM.md for provenance and local changes.

use alloy_primitives::{eip191_hash_message, keccak256};
use cosmwasm_std::{
    entry_point, instantiate2_address, to_json_binary, Addr, CodeInfoResponse, Deps, DepsMut, Env,
    HexBinary, MessageInfo, QueryResponse, Response, StdError, StdResult, Uint256, WasmMsg,
};
use cw2::set_contract_version;
use layer_climb_address::EvmAddr;
use sha3::{Digest, Keccak256};
use wavs_types::contracts::cosmwasm::service_handler::{WavsEnvelope, WavsSignatureData};

use crate::error::ContractError;
use crate::state::{
    Config, CONFIG, OPERATOR_REGISTERED, OPERATOR_TO_SIGNING_KEY, OPERATOR_WEIGHTS, OWNER,
    SIGNING_KEY_TO_OPERATOR, TOTAL_WEIGHT,
};
use app_contract_api::stake_registry::{
    ExecuteMsg, InstantiateMsg, OperatorWeightUpdatedEvent, QueryMsg, SigningKeyUpdateEvent,
    TotalWeightUpdatedEvent, ValidationResult,
};

const CONTRACT_NAME: &str = env!("CARGO_PKG_NAME");
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[entry_point]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    match msg.service_manager_instantiate {
        WasmMsg::Instantiate2 {
            code_id, ref salt, ..
        } => {
            let canonical_creator = deps.api.addr_canonicalize(env.contract.address.as_str())?;
            let CodeInfoResponse { checksum, .. } = deps.querier.query_wasm_code_info(code_id)?;
            let service_manager =
                instantiate2_address(checksum.as_slice(), &canonical_creator, salt)?;
            let service_manager = deps.api.addr_humanize(&service_manager)?;

            let config = Config::new(service_manager);
            CONFIG.save(deps.storage, &config)?;
        }
        _ => {
            return Err(ContractError::Std(StdError::msg(
                "Could not instantiate service manager",
            )));
        }
    }
    let owner = deps.api.addr_validate(&msg.owner)?;
    OWNER.save(deps.storage, &owner)?;
    TOTAL_WEIGHT.save(deps.storage, &Uint256::zero())?;

    Ok(Response::new()
        .add_attribute("method", "instantiate")
        .add_message(msg.service_manager_instantiate))
}

#[entry_point]
pub fn execute(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::TransferOwnership { new_owner } => {
            let owner = OWNER.load(deps.storage)?;
            if info.sender != owner {
                return Err(ContractError::Unauthorized {});
            }
            let new_owner = deps.api.addr_validate(&new_owner)?;
            OWNER.save(deps.storage, &new_owner)?;
            Ok(Response::new()
                .add_attribute("method", "transfer_ownership")
                .add_attribute("old_owner", owner)
                .add_attribute("new_owner", new_owner))
        }
        ExecuteMsg::SetOperatorDetails {
            operator,
            signing_key,
            weight,
        } => execute_set_operator_details(deps, _env, info, operator, signing_key, weight),
        ExecuteMsg::BatchSetOperatorDetails {
            operators,
            signing_keys,
            weights,
        } => execute_batch_set_operator_details(deps, _env, info, operators, signing_keys, weights),
    }
}

#[entry_point]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<QueryResponse> {
    match msg {
        QueryMsg::Owner {} => to_json_binary(&OWNER.load(deps.storage)?),
        QueryMsg::ValidateSignature {
            envelope,
            signature_data,
        } => to_json_binary(&query_validate_signature(deps, envelope, signature_data)?),
        QueryMsg::GetOperatorWeight { operator } => {
            to_json_binary(&query_operator_weight(deps, operator)?)
        }
        QueryMsg::GetOperatorSigningKey { operator } => {
            to_json_binary(&query_operator_signing_key(deps, operator)?)
        }
        QueryMsg::GetLatestOperatorForSigningKey { signing_key } => {
            to_json_binary(&query_latest_operator_for_signing_key(deps, signing_key)?)
        }
        QueryMsg::GetServiceManager {} => to_json_binary(&query_service_manager(deps)?),
        QueryMsg::GetTotalWeight {} => to_json_binary(&query_total_weight(deps)?),
    }
}

fn execute_set_operator_details(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    operator: EvmAddr,
    signing_key: EvmAddr,
    weight: Uint256,
) -> Result<Response, ContractError> {
    let owner = OWNER.load(deps.storage)?;
    if info.sender != owner {
        return Err(ContractError::Unauthorized {});
    }

    let snapshot_height = env.block.height;
    let (mut events, old_total_weight, new_total_weight) =
        set_operator_details_at(deps, snapshot_height, &operator, &signing_key, weight)?;

    // Emit TotalWeightUpdated event
    let total_weight_event = TotalWeightUpdatedEvent {
        old_total_weight,
        new_total_weight,
    };
    events.push(total_weight_event.into());

    Ok(Response::new()
        .add_events(events)
        .add_attribute("method", "set_operator_details")
        .add_attribute("operator", operator.to_string())
        .add_attribute("signing_key", signing_key.to_string())
        .add_attribute("weight", weight.to_string()))
}

fn execute_batch_set_operator_details(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    operators: Vec<EvmAddr>,
    signing_keys: Vec<EvmAddr>,
    weights: Vec<Uint256>,
) -> Result<Response, ContractError> {
    let owner = OWNER.load(deps.storage)?;
    if info.sender != owner {
        return Err(ContractError::Unauthorized {});
    }

    if operators.len() != signing_keys.len() || operators.len() != weights.len() {
        return Err(ContractError::InvalidArrayLengths {});
    }

    let mut all_events = Vec::new();
    let initial_total_weight = TOTAL_WEIGHT.load(deps.storage)?;

    for i in 0..operators.len() {
        let (events, _old_total, _new_total) = set_operator_details_at(
            deps.branch(),
            env.block.height,
            &operators[i],
            &signing_keys[i],
            weights[i],
        )?;
        all_events.extend(events);
    }

    // Emit final TotalWeightUpdated event
    let final_total_weight = TOTAL_WEIGHT.load(deps.storage)?;
    let total_weight_event = TotalWeightUpdatedEvent {
        old_total_weight: initial_total_weight,
        new_total_weight: final_total_weight,
    };
    all_events.push(total_weight_event.into());

    Ok(Response::new()
        .add_events(all_events)
        .add_attribute("method", "batch_set_operator_details")
        .add_attribute("count", operators.len().to_string()))
}

fn set_operator_details_at(
    deps: DepsMut,
    snapshot_height: u64,
    operator: &EvmAddr,
    signing_key: &EvmAddr,
    weight: Uint256,
) -> Result<(Vec<cosmwasm_std::Event>, Uint256, Uint256), ContractError> {
    // Get current weight
    let operator_key = operator.to_string();
    let current_weight = OPERATOR_WEIGHTS
        .may_load(deps.storage, operator_key.clone())?
        .unwrap_or_default();

    // Update operator weight with block height
    OPERATOR_WEIGHTS.save(deps.storage, operator_key.clone(), &weight, snapshot_height)?;

    // Update total weight
    let total_weight = TOTAL_WEIGHT.load(deps.storage)?;
    let new_total_weight = total_weight - current_weight + weight;
    TOTAL_WEIGHT.save(deps.storage, &new_total_weight)?;

    // Update signing key mappings
    let current_signing_key =
        OPERATOR_TO_SIGNING_KEY.may_load(deps.storage, operator_key.clone())?;

    let mut events = Vec::new();

    if current_signing_key.as_ref() != Some(signing_key) {
        // Remove old signing key mapping if it exists
        if let Some(old_key) = current_signing_key.clone() {
            let old_key_str = old_key.to_string();
            SIGNING_KEY_TO_OPERATOR.remove(deps.storage, old_key_str, snapshot_height)?;
        }

        // Set new signing key mapping
        let signing_key_str = signing_key.to_string();
        OPERATOR_TO_SIGNING_KEY.save(
            deps.storage,
            operator_key.clone(),
            signing_key,
            snapshot_height,
        )?;
        SIGNING_KEY_TO_OPERATOR.save(deps.storage, signing_key_str, operator, snapshot_height)?;

        // Emit SigningKeyUpdate event
        let signing_key_event = SigningKeyUpdateEvent {
            operator: operator.clone(),
            block_number: snapshot_height,
            new_signing_key: signing_key.clone(),
            old_signing_key: current_signing_key.clone(),
        };
        events.push(signing_key_event.into());
    }

    // Mark operator as registered
    OPERATOR_REGISTERED.save(deps.storage, operator_key, &true)?;

    // Emit OperatorWeightUpdated event
    let weight_event = OperatorWeightUpdatedEvent {
        operator: operator.clone(),
        old_weight: current_weight,
        new_weight: weight,
    };
    events.push(weight_event.into());

    Ok((events, total_weight, new_total_weight))
}

fn query_validate_signature(
    deps: Deps,
    envelope: WavsEnvelope,
    signature_data: WavsSignatureData,
) -> StdResult<ValidationResult> {
    let total_weight = TOTAL_WEIGHT.load(deps.storage)?;
    let mut voting_power_signed = Uint256::zero();

    // Basic sanity checks to avoid panics and invalid data
    if signature_data.signers.is_empty()
        || signature_data.signers.len() != signature_data.signatures.len()
    {
        return Err(StdError::msg(format!(
            "mismatched signer and signatures length. Signers={}, Signatures={}",
            signature_data.signers.len(),
            signature_data.signatures.len()
        )));
    }

    // Enforce unique signers to prevent double counting
    use std::collections::HashSet;
    let mut seen_signers: HashSet<[u8; 20]> = HashSet::new();

    // Verify each signature (operators are actually signing keys in the decoded data)
    for (i, signing_key) in signature_data.signers.iter().enumerate() {
        // Reject zero address signers
        if signing_key.as_bytes().iter().all(|b| *b == 0) {
            return Err(StdError::msg("signing key address is zero"));
        }

        // Reject duplicate signer entries
        let signer_arr: [u8; 20] = signing_key.as_bytes();
        if !seen_signers.insert(signer_arr) {
            return Err(StdError::msg(format!(
                "duplicate signing key address: {signing_key}"
            )));
        }

        // Authorization is evaluated at execution time. A caller-controlled
        // reference block must not restore a replaced key's authority.
        let signing_key_str = signing_key.to_string();
        let operator = SIGNING_KEY_TO_OPERATOR
            .may_load(deps.storage, signing_key_str)?
            .ok_or_else(|| StdError::msg("Signer not registered"))?;

        // Verify signature using the signing key (safe index: len equality checked above)
        let signature = &signature_data.signatures[i];
        if !is_valid_signature(deps, &envelope, signature, signing_key)? {
            return Err(StdError::msg(format!(
                "Invalid signature from signing key: {signing_key}"
            )));
        }

        // Use current weights consistently with the current total weight.
        let operator_key = operator.to_string();
        let operator_weight = OPERATOR_WEIGHTS
            .may_load(deps.storage, operator_key)?
            .unwrap_or_default();
        if operator_weight.is_zero() {
            return Err(StdError::msg(format!(
                "operator {operator} has zero weight"
            )));
        }
        voting_power_signed += operator_weight;
    }

    Ok(ValidationResult {
        total_voting_power: total_weight,
        voting_power_signed,
        reference_block: signature_data.reference_block,
    })
}

// Mimics Solidity's signer.isValidSignatureNow(digest, signature)
fn is_valid_signature(
    deps: Deps,
    envelope: &WavsEnvelope,
    signature: &HexBinary,
    signer_address: &EvmAddr,
) -> StdResult<bool> {
    // Validate signature length (must be 65 bytes for ECDSA)
    if signature.len() != 65 {
        return Ok(false);
    }

    // Additional validation: signature must not be zero
    if signature.as_slice().iter().all(|&b| b == 0) {
        return Ok(false);
    }

    let sig_bytes = signature.as_slice();
    let rs = &sig_bytes[0..64];
    let recovery_id = sig_bytes[64];
    // Create recovery ID (normalize from Ethereum format 27/28 to k256 format 0/1)
    let normalized_recovery_id = match recovery_id {
        27 => 0,
        28 => 1,
        0 | 1 => recovery_id,
        _ => {
            return Ok(false);
        }
    };

    let hash = eip191_hash_message(keccak256(envelope.as_slice()));

    let calculated_pubkey =
        deps.api
            .secp256k1_recover_pubkey(hash.as_slice(), rs, normalized_recovery_id)?;

    // Verify the signature is cryptographically valid
    let cryptographically_valid =
        deps.api
            .secp256k1_verify(hash.as_slice(), rs, &calculated_pubkey)?;

    if !cryptographically_valid {
        return Ok(false);
    }

    // Convert recovered public key to Ethereum address
    let recovered_address = ethereum_address_raw(&calculated_pubkey)?;

    // Check if the recovered address matches the expected signer
    Ok(recovered_address == signer_address.as_bytes())
}

pub fn ethereum_address_raw(pubkey: &[u8]) -> StdResult<[u8; 20]> {
    let (tag, data) = match pubkey.split_first() {
        Some(pair) => pair,
        None => return Err(StdError::msg("Public key must not be empty")),
    };
    if *tag != 0x04 {
        return Err(StdError::msg("Public key must start with 0x04"));
    }
    if data.len() != 64 {
        return Err(StdError::msg("Public key must be 65 bytes long"));
    }

    let hash = Keccak256::digest(data);
    Ok(hash[hash.len() - 20..].try_into().unwrap())
}

fn query_operator_weight(deps: Deps, operator: EvmAddr) -> StdResult<Uint256> {
    let operator_key = operator.to_string();
    OPERATOR_WEIGHTS
        .may_load(deps.storage, operator_key)
        .map(|w| w.unwrap_or_default())
}

fn query_operator_signing_key(deps: Deps, operator: EvmAddr) -> StdResult<Option<EvmAddr>> {
    let operator_key = operator.to_string();
    OPERATOR_TO_SIGNING_KEY.may_load(deps.storage, operator_key)
}

fn query_latest_operator_for_signing_key(
    deps: Deps,
    signing_key: EvmAddr,
) -> StdResult<Option<EvmAddr>> {
    let signing_key_str = signing_key.to_string();
    SIGNING_KEY_TO_OPERATOR.may_load(deps.storage, signing_key_str)
}

fn query_service_manager(deps: Deps) -> StdResult<Addr> {
    let config = CONFIG.load(deps.storage)?;
    Ok(config.service_manager)
}

fn query_total_weight(deps: Deps) -> StdResult<Uint256> {
    TOTAL_WEIGHT.load(deps.storage)
}
