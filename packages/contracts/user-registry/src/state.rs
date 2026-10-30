use app_contract_api::user_registry::msg::{InstantiateMsg, ProxyConfig, UserId};
use cosmwasm_std::{Addr, DepsMut, StdResult, Storage};
use cw2::set_contract_version;
use cw_storage_plus::{Item, Map};

use crate::error::ContractError;

const CONTRACT_NAME: &str = "crates.io:user-registry";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");
const ADMINS: Item<Vec<Addr>> = Item::new("admins");
pub const OWNER: Item<Addr> = Item::new("owner");
pub const PROXY_CONFIG: Item<ProxyConfig> = Item::new("proxy-config");
const USER_PROXY_ADDRS: Map<UserId, Addr> = Map::new("user-proxy-addrs");

pub fn init(deps: &mut DepsMut, msg: &InstantiateMsg) -> Result<(), ContractError> {
    let admins = msg
        .admins
        .iter()
        .map(|addr| deps.api.addr_validate(addr))
        .collect::<StdResult<Vec<_>>>()?;

    if admins.is_empty() {
        return Err(ContractError::NoAdmins {});
    }

    ADMINS.save(deps.storage, &admins)?;
    OWNER.save(deps.storage, &deps.api.addr_validate(&msg.owner)?)?;
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    Ok(())
}

pub fn get_admins(store: &dyn Storage) -> StdResult<Vec<Addr>> {
    ADMINS.load(store)
}

pub fn update_admins(deps: DepsMut, admins: Vec<String>) -> Result<(), ContractError> {
    if admins.is_empty() {
        return Err(ContractError::NoAdmins {});
    }
    let admins = admins
        .iter()
        .map(|a| deps.api.addr_validate(a))
        .collect::<StdResult<Vec<_>>>()?;
    ADMINS.save(deps.storage, &admins)?;
    Ok(())
}

pub fn ensure_owner(store: &dyn Storage, addr: &Addr) -> Result<(), ContractError> {
    if OWNER.load(store)? != addr {
        return Err(ContractError::Unauthorized {});
    }
    Ok(())
}

pub fn ensure_admin(store: &dyn Storage, addr: &Addr) -> Result<(), ContractError> {
    let admins = ADMINS.load(store)?;

    if !admins.iter().any(|a| a == addr) {
        Err(ContractError::Unauthorized {})
    } else {
        Ok(())
    }
}

pub fn register_user(
    deps: DepsMut,
    user_id: UserId,
    proxy_address: Addr,
) -> Result<(), ContractError> {
    if USER_PROXY_ADDRS
        .may_load(deps.storage, user_id.clone())?
        .is_some()
    {
        return Err(ContractError::UserAlreadyRegistered { user_id });
    }

    USER_PROXY_ADDRS.save(deps.storage, user_id, &proxy_address)?;

    Ok(())
}

pub fn get_proxy_address(store: &dyn Storage, user_id: UserId) -> Result<Addr, ContractError> {
    USER_PROXY_ADDRS
        .may_load(store, user_id.clone())?
        .ok_or_else(|| ContractError::ProxyAddressNotFound { user_id })
}
