//! Mock inflow vault contract for testing
pub use app_contract_api::vault::{DenomMetadata, InstantiateMsg};
use cosmwasm_std::{
    to_json_binary, Binary, Deps, DepsMut, Empty, Env, MessageInfo, Response, StdError, StdResult,
};
use cw_multi_test::{Contract, ContractWrapper};
use cw_storage_plus::Item;
use hydro_interface::inflow_vault::{Config, ConfigResponse, ExecuteMsg, QueryMsg};

const CONFIG: Item<Config> = Item::new("config");

fn instantiate(
    deps: DepsMut,
    env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> StdResult<Response> {
    // Mock creates vault_shares_denom from subdenom (real vault uses tokenfactory)
    let vault_shares_denom = format!("factory/{}/{}", env.contract.address, msg.subdenom);
    let config = Config {
        deposit_denom: msg.deposit_denom,
        vault_shares_denom,
        control_center_contract: deps.api.addr_validate(&msg.control_center_contract)?,
        token_info_provider_contract: msg
            .token_info_provider_contract
            .map(|s| deps.api.addr_validate(&s))
            .transpose()?,
        max_withdrawals_per_user: msg.max_withdrawals_per_user,
    };
    CONFIG.save(deps.storage, &config)?;
    Ok(Response::new())
}

fn execute(_deps: DepsMut, _env: Env, _info: MessageInfo, msg: ExecuteMsg) -> StdResult<Response> {
    match msg {
        ExecuteMsg::Deposit { .. } => Ok(Response::new().add_attribute("action", "deposit")),
        _ => Err(StdError::msg("not implemented")),
    }
}

fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => {
            let config = CONFIG.load(deps.storage)?;
            to_json_binary(&ConfigResponse { config })
        }
        _ => Err(StdError::msg("not implemented")),
    }
}

pub fn contract() -> Box<dyn Contract<Empty>> {
    Box::new(ContractWrapper::new(execute, instantiate, query))
}
