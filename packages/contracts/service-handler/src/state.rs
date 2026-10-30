use app_contract_api::{
    service_handler::msg::{Auth, EmailMessageOnly, InstantiateMsg, UserIdEmail},
    user_registry::msg::{ProxyAddressResponse, QueryMsg as UserRegistryQueryMsg, UserId},
};
use cosmwasm_std::{Addr, Deps, DepsMut, Order, StdResult, Storage};
use cw2::set_contract_version;
use cw_storage_plus::{Bound, Item, Map};

const CONTRACT_NAME: &str = env!("CARGO_PKG_NAME");
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

const DEFAULT_QUERY_LIMIT: u32 = 30;
const MAX_QUERY_LIMIT: u32 = 100;

fn query_limit(limit: Option<u32>) -> usize {
    limit.unwrap_or(DEFAULT_QUERY_LIMIT).min(MAX_QUERY_LIMIT) as usize
}

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

    let take_limit = query_limit(limit);

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
    let iter = EMAILS_FROM.prefix(from).range(
        store,
        start_after.map(Bound::exclusive),
        None,
        Order::Ascending,
    );

    let take_limit = query_limit(limit);

    let emails = iter
        .take(take_limit)
        .map(|item| item.map(|(id, email)| (email, id)))
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

    let take_limit = query_limit(limit);

    let emails = iter
        .take(take_limit)
        .map(|item| item.map(|(id, email)| (email, id)))
        .collect::<StdResult<Vec<(UserIdEmail, u64)>>>()?;

    Ok(emails)
}

pub fn migrate(storage: &mut dyn Storage) -> StdResult<()> {
    set_contract_version(storage, CONTRACT_NAME, CONTRACT_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::testing::MockStorage;

    fn storage() -> MockStorage {
        let mut store = MockStorage::new();
        EMAIL_PAGINATION_ID_COUNT.save(&mut store, &0).unwrap();
        store
    }

    fn add(store: &mut dyn Storage, sender: &str, subject: &str) -> u64 {
        push_email(
            store,
            &UserIdEmail {
                from: UserId::new_raw(sender.to_owned()),
                subject: subject.to_owned(),
            },
        )
        .unwrap()
    }

    #[test]
    fn sender_queries_stay_within_prefix_across_pages() {
        let mut store = storage();
        let first = add(&mut store, "alice", "alice first");
        let bob = add(&mut store, "bob", "bob only");
        let second = add(&mut store, "alice", "alice second");
        let page = list_emails_from(&store, "alice", None, Some(1)).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].0.subject, "alice first");
        assert_eq!(page[0].1, first);
        let page = list_emails_from(&store, "alice", Some(first), Some(1)).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].0.subject, "alice second");
        assert_eq!(page[0].1, second);
        assert!(list_emails_from(&store, "alice", Some(second), None)
            .unwrap()
            .is_empty());
        let page = list_emails_from(&store, "bob", None, None).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].0.subject, "bob only");
        assert_eq!(page[0].1, bob);
        assert!(list_emails_from(&store, "absent", None, None)
            .unwrap()
            .is_empty());
        assert!(list_emails_from(&store, "absent", Some(first), None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn all_list_queries_apply_default_maximum_and_zero_limits() {
        let mut store = storage();
        for i in 0..125 {
            add(&mut store, "alice", &format!("alice {i}"));
            add(&mut store, &format!("user{i:03}"), "other sender");
        }
        for (limit, expected) in [
            (None, 30),
            (Some(u32::MAX), 100),
            (Some(1), 1),
            (Some(0), 0),
        ] {
            assert_eq!(
                list_email_user_ids(&store, None, limit).unwrap().len(),
                expected
            );
            assert_eq!(
                list_emails_from(&store, "alice", None, limit)
                    .unwrap()
                    .len(),
                expected
            );
            assert_eq!(list_emails(&store, None, limit).unwrap().len(), expected);
        }
    }

    #[test]
    fn capped_queries_can_page_through_every_record() {
        let mut store = storage();
        for i in 0..125 {
            add(&mut store, &format!("user{i:03}"), "deposit");
        }
        let first = list_email_user_ids(&store, None, Some(u32::MAX)).unwrap();
        let rest = list_email_user_ids(&store, first.last(), Some(u32::MAX)).unwrap();
        assert_eq!(first.len(), 100);
        assert_eq!(rest.len(), 25);
        assert_eq!(rest[0].as_str(), "user100");
        let first = list_emails(&store, None, Some(u32::MAX)).unwrap();
        let rest = list_emails(&store, Some(first.last().unwrap().1), Some(u32::MAX)).unwrap();
        assert_eq!(first.len(), 100);
        assert_eq!(rest.len(), 25);
        assert_eq!(rest[0].1, 101);
    }
}
