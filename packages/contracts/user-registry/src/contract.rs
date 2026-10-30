use app_contract_api::user_registry::{
    event::UserRegisteredEvent,
    msg::{
        ConfigResponse, ExecuteMsg, InstantiateMsg, ProxyAddressResponse, ProxyConfig, QueryMsg,
    },
};
use cosmwasm_std::{
    ensure, entry_point, instantiate2_address, to_json_binary, Binary, Deps, DepsMut, Env,
    MessageInfo, Response, StdError, StdResult, WasmMsg,
};

use crate::{error::ContractError, state};
use app_contract_api::proxy::{ProxyConfigResponse, ProxyInstantiateMsg, ProxyQueryMsg};

#[entry_point]
pub fn instantiate(
    mut deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    state::init(&mut deps, &msg)?;

    Ok(Response::new().add_attribute("action", "instantiate_user_registry"))
}

#[entry_point]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::RegisterUser { user_id } => {
            state::ensure_admin(deps.storage, &info.sender)?;
            ensure!(
                info.funds.is_empty(),
                StdError::msg("Registration does not accept funds")
            );
            let config = state::PROXY_CONFIG.load(deps.storage)?;
            let salt = user_id.proxy_salt()?;
            let checksum = deps
                .querier
                .query_wasm_code_info(config.proxy_code_id)?
                .checksum;
            let creator = deps.api.addr_canonicalize(env.contract.address.as_str())?;
            let canonical = instantiate2_address(checksum.as_slice(), &creator, &salt)
                .map_err(StdError::msg)?;
            let proxy_address = deps.api.addr_humanize(&canonical)?;
            // Only this registry can create a contract at this deterministic
            // address. If one exists, still check it before reusing it.
            let existing = deps.querier.query_wasm_contract_info(&proxy_address).ok();
            if let Some(info) = &existing {
                ensure!(
                    info.code_id == config.proxy_code_id
                        && info.creator == env.contract.address
                        && info.admin.is_none(),
                    StdError::msg("Unexpected proxy configuration")
                );
                let proxy: ProxyConfigResponse = deps
                    .querier
                    .query_wasm_smart(&proxy_address, &ProxyQueryMsg::Config {})?;
                ensure!(
                    proxy.config.admins == vec![config.service_handler.to_string()]
                        && proxy.config.control_center_registry
                            == config.control_center_registry.as_str(),
                    StdError::msg("Unexpected proxy configuration")
                );
            }
            let instantiate = WasmMsg::Instantiate2 {
                admin: None,
                code_id: config.proxy_code_id,
                label: "Hydro Email Proxy".into(),
                msg: to_json_binary(&ProxyInstantiateMsg {
                    admins: vec![config.service_handler.to_string()],
                    control_center_registry: config.control_center_registry.to_string(),
                })?,
                funds: vec![],
                salt: salt.into(),
            };
            // Proxy creation and mapping are committed together or both rolled back.
            state::register_user(deps, user_id.clone(), proxy_address.clone())?;

            let response = Response::new().add_event(UserRegisteredEvent {
                user_id,
                proxy_address,
            });
            Ok(if existing.is_some() {
                response
            } else {
                response.add_message(instantiate)
            })
        }
        ExecuteMsg::ConfigureProxy {
            proxy_code_id,
            service_handler,
            control_center_registry,
        } => {
            state::ensure_owner(deps.storage, &info.sender)?;
            ensure!(
                !state::PROXY_CONFIG.exists(deps.storage),
                StdError::msg("Proxy configuration is immutable")
            );
            deps.querier.query_wasm_code_info(proxy_code_id)?;
            let service_handler = deps.api.addr_validate(&service_handler)?;
            let control_center_registry = deps.api.addr_validate(&control_center_registry)?;
            deps.querier.query_wasm_contract_info(&service_handler)?;
            deps.querier
                .query_wasm_contract_info(&control_center_registry)?;
            state::PROXY_CONFIG.save(
                deps.storage,
                &ProxyConfig {
                    proxy_code_id,
                    service_handler,
                    control_center_registry,
                },
            )?;
            Ok(Response::new().add_attribute("action", "configure_proxy"))
        }
        ExecuteMsg::UpdateAdmins { admins } => {
            state::ensure_owner(deps.storage, &info.sender)?;
            state::update_admins(deps, admins)?;
            Ok(Response::new().add_attribute("action", "update_admins"))
        }
        ExecuteMsg::TransferOwnership { owner } => {
            state::ensure_owner(deps.storage, &info.sender)?;
            state::OWNER.save(deps.storage, &deps.api.addr_validate(&owner)?)?;
            Ok(Response::new().add_attribute("action", "transfer_ownership"))
        }
    }
}

#[entry_point]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::ProxyAddress { user_id } => {
            let address = state::get_proxy_address(deps.storage, user_id)?;
            to_json_binary(&ProxyAddressResponse { address })
        }
        QueryMsg::Config {} => to_json_binary(&ConfigResponse {
            owner: state::OWNER.load(deps.storage)?,
            admins: state::get_admins(deps.storage)?,
            proxy: state::PROXY_CONFIG.may_load(deps.storage)?,
        }),
    }
}
