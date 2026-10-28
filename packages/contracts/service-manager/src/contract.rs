// SPDX-License-Identifier: GPL-3.0-or-later
// Derived from Lay3rLabs/cw-middleware v0.2.0-alpha.15, Layer Labs (Cayman).
// See ../UPSTREAM.md for provenance and local changes.

use cosmwasm_std::{
    entry_point, to_json_binary, Deps, DepsMut, Env, MessageInfo, QueryResponse, Response,
    StdResult, Uint256,
};
use cw2::set_contract_version;
use layer_climb_address::EvmAddr;
use wavs_types::contracts::cosmwasm::{
    service_handler::{WavsEnvelope, WavsSignatureData},
    service_manager::{
        error::WavsValidateError, event::WavsServiceUriUpdatedEvent, ServiceManagerExecuteMessages,
        ServiceManagerQueryMessages, WavsValidateResult,
    },
};

use crate::state::{self, ADMIN, QUORUM_DENOMINATOR, QUORUM_NUMERATOR, STAKE_REGISTRY};
use app_contract_api::{
    service_manager::{ExecuteMsg, InstantiateMsg, QueryMsg},
    stake_registry::ValidationResult,
};

// version info for migration info
const CONTRACT_NAME: &str = env!("CARGO_PKG_NAME");
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[entry_point]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> StdResult<Response> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    let admin = deps.api.addr_validate(&msg.admin)?;
    ADMIN.save(deps.storage, &admin)?;
    STAKE_REGISTRY.save(deps.storage, &info.sender)?;
    state::SERVICE_URI.save(deps.storage, &String::new())?;

    // Set default quorum configuration (2/3)
    let default_numerator = Uint256::from(2u128);
    let default_denominator = Uint256::from(3u128);
    QUORUM_NUMERATOR.save(deps.storage, &default_numerator)?;
    QUORUM_DENOMINATOR.save(deps.storage, &default_denominator)?;

    Ok(Response::default()
        .add_attribute("admin", admin)
        .add_attribute("quorum_numerator", default_numerator.to_string())
        .add_attribute("quorum_denominator", default_denominator.to_string()))
}

#[entry_point]
pub fn execute(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> StdResult<Response> {
    match msg {
        ExecuteMsg::TransferAdmin { new_admin } => {
            let admin = ADMIN.load(deps.storage)?;
            if info.sender != admin {
                return Err(cosmwasm_std::StdError::msg(
                    "Unauthorized: only admin can transfer administration",
                ));
            }
            let new_admin = deps.api.addr_validate(&new_admin)?;
            ADMIN.save(deps.storage, &new_admin)?;
            Ok(Response::new()
                .add_attribute("method", "transfer_admin")
                .add_attribute("old_admin", admin)
                .add_attribute("new_admin", new_admin))
        }
        ExecuteMsg::Wavs(msg) => match msg {
            ServiceManagerExecuteMessages::WavsSetQuorumThreshold {
                numerator,
                denominator,
            } => {
                let admin = ADMIN.load(deps.storage)?;
                if info.sender != admin {
                    return Err(cosmwasm_std::StdError::msg(
                        "Unauthorized: only admin can set quorum threshold",
                    ));
                }

                // Validate quorum parameters
                if numerator.is_zero() || denominator.is_zero() || numerator > denominator {
                    WavsValidateResult::Err(WavsValidateError::InvalidQuorumParameters)
                        .into_std()?
                }

                QUORUM_NUMERATOR.save(deps.storage, &numerator)?;
                QUORUM_DENOMINATOR.save(deps.storage, &denominator)?;

                Ok(Response::new()
                    .add_attribute("method", "wavs_set_quorum_threshold")
                    .add_attribute("numerator", numerator.to_string())
                    .add_attribute("denominator", denominator.to_string()))
            }
            ServiceManagerExecuteMessages::WavsSetServiceUri { service_uri } => {
                let admin = ADMIN.load(deps.storage)?;
                if info.sender != admin {
                    return Err(cosmwasm_std::StdError::msg(
                        "Unauthorized: only admin can set service URI",
                    ));
                }

                state::SERVICE_URI.save(deps.storage, &service_uri)?;

                Ok(Response::new().add_event(WavsServiceUriUpdatedEvent { service_uri }))
            }
        },
    }
}

#[entry_point]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<QueryResponse> {
    match msg {
        QueryMsg::Mirror(msg) => match msg {
            app_contract_api::service_manager::MirrorServiceManagerQueryMessages::Admin {} => {
                let admin = ADMIN.load(deps.storage)?;
                to_json_binary(&admin)
            },
            app_contract_api::service_manager::MirrorServiceManagerQueryMessages::StakeRegistry {  } => {
                let stake_registry = STAKE_REGISTRY.load(deps.storage)?;
                to_json_binary(&stake_registry)
            }
        },
        QueryMsg::Wavs(msg) => match msg {
            ServiceManagerQueryMessages::WavsQuorumThreshold {} => {
                let numerator = QUORUM_NUMERATOR.load(deps.storage)?;
                let denominator = QUORUM_DENOMINATOR.load(deps.storage)?;
                let threshold = wavs_types::contracts::cosmwasm::service_manager::QuorumThreshold {
                    numerator,
                    denominator,
                };
                to_json_binary(&threshold)
            }
            ServiceManagerQueryMessages::WavsOperatorWeight { operator_address } => {
                // Query stake registry for operator weight
                let stake_registry = state::STAKE_REGISTRY.load(deps.storage)?;
                let weight: Uint256 = deps.querier.query_wasm_smart(
                    stake_registry,
                    &app_contract_api::stake_registry::QueryMsg::GetOperatorWeight {
                        operator: operator_address,
                    },
                )?;
                to_json_binary(&weight)
            }
            ServiceManagerQueryMessages::WavsValidate {
                envelope,
                signature_data,
            } => {
                let validation_result = wavs_validate(deps, envelope, signature_data)?;

                to_json_binary(&validation_result)
            }
            ServiceManagerQueryMessages::WavsServiceUri {} => {
                to_json_binary(&state::SERVICE_URI.load(deps.storage)?)
            }
            ServiceManagerQueryMessages::WavsLatestOperatorForSigningKey { signing_key_addr } => {
                // Query stake registry for operator by signing key
                let stake_registry = state::STAKE_REGISTRY.load(deps.storage)?;
                let operator: Option<EvmAddr> = deps.querier.query_wasm_smart(
                    stake_registry,
                    &app_contract_api::stake_registry::QueryMsg::GetLatestOperatorForSigningKey {
                        signing_key: signing_key_addr,
                    },
                )?;
                to_json_binary(&operator)
            }
        },
    }
}

pub fn wavs_validate(
    deps: Deps,
    envelope: WavsEnvelope,
    signature_data: WavsSignatureData,
) -> StdResult<WavsValidateResult> {
    // Input validation
    if signature_data.signers.is_empty()
        || signature_data.signers.len() != signature_data.signatures.len()
    {
        return Ok(WavsValidateResult::Err(
            WavsValidateError::InvalidSignatureLength,
        ));
    }
    // Validate signatures via stake registry, if configured
    let stake_registry = state::STAKE_REGISTRY.load(deps.storage)?;

    // Query stake registry for signature validation and weight calculation
    match deps
        .querier
        .query_wasm_smart::<ValidationResult>(
            stake_registry,
            &app_contract_api::stake_registry::QueryMsg::ValidateSignature {
                envelope,
                signature_data,
            },
        )
        .map_err(|e| WavsValidateResult::Err(WavsValidateError::InvalidSignature(e.to_string())))
    {
        Ok(ValidationResult {
            voting_power_signed,
            total_voting_power,
            ..
        }) => validate_quorum(voting_power_signed, total_voting_power, &deps),
        Err(e) => Ok(e),
    }
}

/// Validates that sufficient quorum has been reached
fn validate_quorum(
    signed_weight: Uint256,
    total_weight: Uint256,
    deps: &Deps,
) -> StdResult<WavsValidateResult> {
    // Load quorum configuration
    let numerator = QUORUM_NUMERATOR.load(deps.storage)?;
    let denominator = QUORUM_DENOMINATOR.load(deps.storage)?;

    // Calculate threshold weight: (total_weight * numerator) / denominator
    let threshold_weight =
        total_weight.full_mul(numerator) / cosmwasm_std::Uint512::from(denominator);
    // Convert threshold_weight from Uint512 to Uint256 (safely)
    let threshold_weight = threshold_weight
        .try_into()
        .unwrap_or(cosmwasm_std::Uint256::MAX);

    // Avoid 0 weight ever passing this check
    if total_weight.is_zero() {
        return Ok(WavsValidateResult::Err(
            WavsValidateError::InsufficientQuorumZero,
        ));
    }

    // Check if signed_weight >= threshold_weight
    if signed_weight < threshold_weight {
        return Ok(WavsValidateResult::Err(
            WavsValidateError::InsufficientQuorum {
                signer_weight: signed_weight,
                threshold_weight,
                total_weight,
            },
        ));
    }

    Ok(WavsValidateResult::Ok)
}
