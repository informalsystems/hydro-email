use app_contract_api::{
    service_handler::msg::{Auth, EmailMessageOnly, InstantiateMsg, UserIdEmail},
    user_registry::msg::{ProxyAddressResponse, QueryMsg as UserRegistryQueryMsg, UserId},
};
use cosmwasm_std::{Addr, Deps, DepsMut, Order, StdResult, Storage};
use cw2::set_contract_version;
use cw_storage_plus::{Bound, Item, Map};

const CONTRACT_NAME: &str = env!("CARGO_PKG_NAME");
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Only set if we take ServiceHandler interface
pub const SERVICE_MANAGER: Item<Addr> = Item::new("service-manager");
/// Only set in the test approach
pub const ADMIN: Item<Addr> = Item::new("admin");
/// Tracks processed WAVS event IDs
pub const PROCESSED_EVENTS: Map<&[u8], ()> = Map::new("processed-events");
/// User Registry contract address
const USER_REGISTRY_ADDRESS: Item<Addr> = Item::new("user-registry-address");

const EMAILS_FROM: Map<(&str, u64), EmailMessageOnly> = Map::new("emails-from");
const EMAILS_IN_ORDER: Map<u64, UserIdEmail> = Map::new("emails-in-order");
const EMAIL_USER_IDS: Map<&str, ()> = Map::new("email-user-ids");
const EMAIL_PAGINATION_ID_COUNT: Item<u64> = Item::new("email-pagination-id-count");

pub fn initialize(deps: &mut DepsMut, msg: InstantiateMsg) -> StdResult<()> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    // Set admin or service manager for later validation
    match msg.auth {
        Auth::ServiceManager(service_manager) => {
            let service_manager_addr = deps.api.addr_validate(&service_manager)?;
            SERVICE_MANAGER.save(deps.storage, &service_manager_addr)?;
        }
        Auth::Admin(admin) => {
            let admin_addr = deps.api.addr_validate(&admin)?;
            ADMIN.save(deps.storage, &admin_addr)?;
        }
    }

    EMAIL_PAGINATION_ID_COUNT.save(deps.storage, &0u64)?;
    USER_REGISTRY_ADDRESS.save(deps.storage, &deps.api.addr_validate(&msg.user_registry)?)?;

    Ok(())
}

pub fn user_registry_address(store: &dyn Storage) -> StdResult<Addr> {
    USER_REGISTRY_ADDRESS.load(store)
}

pub fn proxy_address(deps: Deps, user_id: UserId) -> StdResult<Addr> {
    let user_registry_addr = user_registry_address(deps.storage)?;

    let resp = deps.querier.query_wasm_smart::<ProxyAddressResponse>(
        user_registry_addr,
        &UserRegistryQueryMsg::ProxyAddress { user_id },
    )?;

    Ok(resp.address)
}

pub fn push_email(store: &mut dyn Storage, email: &UserIdEmail) -> StdResult<u64> {
    let pagination_id =
        EMAIL_PAGINATION_ID_COUNT.update(store, |id| -> StdResult<u64> { Ok(id + 1) })?;

    EMAILS_FROM.save(
        store,
        (&email.from.to_string(), pagination_id),
        &email.into(),
    )?;
    EMAILS_IN_ORDER.save(store, pagination_id, email)?;
    EMAIL_USER_IDS.save(store, &email.from.to_string(), &())?;

    Ok(pagination_id)
}

pub fn list_email_user_ids(
    store: &dyn Storage,
    start_after: Option<&UserId>,
    limit: Option<u32>,
) -> StdResult<Vec<UserId>> {
    let iter = EMAIL_USER_IDS.range(
        store,
        start_after.map(|x| Bound::exclusive(x.as_str())),
        None,
        Order::Ascending,
    );

    let take_limit = limit.unwrap_or(u32::MAX) as usize;

    let addrs = iter
        .take(take_limit)
        .map(|item| item.map(|(addr, _)| UserId::new_raw(addr.to_string())))
        .collect::<StdResult<Vec<UserId>>>()?;

    Ok(addrs)
}

pub fn list_emails_from(
    store: &dyn Storage,
    from: &str,
    start_after: Option<u64>,
    limit: Option<u32>,
) -> StdResult<Vec<(EmailMessageOnly, u64)>> {
    let iter = EMAILS_FROM.range(
        store,
        start_after.map(|id| Bound::exclusive((from, id))),
        None,
        Order::Ascending,
    );

    let take_limit = limit.unwrap_or(u32::MAX) as usize;

    let emails = iter
        .take(take_limit)
        .map(|item| item.map(|((_, id), email)| (email, id)))
        .collect::<StdResult<Vec<(EmailMessageOnly, u64)>>>()?;

    Ok(emails)
}

pub fn list_emails(
    store: &dyn Storage,
    start_after: Option<u64>,
    limit: Option<u32>,
) -> StdResult<Vec<(UserIdEmail, u64)>> {
    let iter = EMAILS_IN_ORDER.range(
        store,
        start_after.map(Bound::exclusive),
        None,
        Order::Ascending,
    );

    let take_limit = limit.unwrap_or(u32::MAX) as usize;

    let emails = iter
        .take(take_limit)
        .map(|item| item.map(|(id, email)| (email, id)))
        .collect::<StdResult<Vec<(UserIdEmail, u64)>>>()?;

    Ok(emails)
}

pub fn migrate(storage: &mut dyn Storage) -> StdResult<()> {
    set_contract_version(storage, CONTRACT_NAME, CONTRACT_VERSION)
}
