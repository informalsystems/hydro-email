use app_contract_api::user_registry::msg::{
    ConfigResponse, ExecuteMsg, InstantiateMsg, ProxyAddressResponse, QueryMsg, UserId,
};
use cosmwasm_std::{
    instantiate2_address, to_json_binary, Addr, Api, Binary, Deps, DepsMut, Empty, Env,
    MessageInfo, Response, StdError, StdResult,
};
use cw_multi_test::{App, ContractWrapper, Executor};

fn stub_init(_: DepsMut, _: Env, _: MessageInfo, _: Empty) -> StdResult<Response> {
    Ok(Response::new())
}
fn stub_execute(_: DepsMut, _: Env, _: MessageInfo, _: Empty) -> StdResult<Response> {
    Ok(Response::new())
}
fn stub_query(_: Deps, _: Env, _: Empty) -> StdResult<Binary> {
    to_json_binary(&Empty {})
}
fn fail_init(
    _: DepsMut,
    _: Env,
    _: MessageInfo,
    _: hydro_proxy::msg::InstantiateMsg,
) -> StdResult<Response> {
    Err(StdError::msg("proxy creation failed"))
}

struct Setup {
    app: App,
    owner: Addr,
    api: Addr,
    registry: Addr,
    handler: Addr,
    centers: Addr,
    proxy_code: u64,
}

impl Setup {
    fn new(failing_proxy: bool) -> Self {
        let mut app = App::default();
        let owner = app.api().addr_make("dao");
        let api = app.api().addr_make("backend");
        let code = app.store_code(Box::new(ContractWrapper::new(
            stub_execute,
            stub_init,
            stub_query,
        )));
        let handler = app
            .instantiate_contract(code, owner.clone(), &Empty {}, &[], "handler", None)
            .unwrap();
        let centers = app
            .instantiate_contract(code, owner.clone(), &Empty {}, &[], "centers", None)
            .unwrap();
        let proxy_code = if failing_proxy {
            app.store_code(Box::new(ContractWrapper::new(
                stub_execute,
                fail_init,
                stub_query,
            )))
        } else {
            app.store_code(Box::new(
                ContractWrapper::new(
                    hydro_proxy::contract::execute,
                    hydro_proxy::contract::instantiate,
                    hydro_proxy::contract::query,
                )
                .with_reply(hydro_proxy::contract::reply),
            ))
        };
        let code = app.store_code(Box::new(ContractWrapper::new(
            app_contract_user_registry::execute,
            app_contract_user_registry::instantiate,
            app_contract_user_registry::query,
        )));
        let registry = app
            .instantiate_contract(
                code,
                owner.clone(),
                &InstantiateMsg {
                    owner: owner.to_string(),
                    admins: vec![api.to_string()],
                },
                &[],
                "registry",
                None,
            )
            .unwrap();
        Self {
            app,
            owner,
            api,
            registry,
            handler,
            centers,
            proxy_code,
        }
    }
    fn configuration(&self) -> ExecuteMsg {
        ExecuteMsg::ConfigureProxy {
            proxy_code_id: self.proxy_code,
            service_handler: self.handler.to_string(),
            control_center_registry: self.centers.to_string(),
        }
    }
    fn configure(&mut self) {
        self.app
            .execute_contract(
                self.owner.clone(),
                self.registry.clone(),
                &self.configuration(),
                &[],
            )
            .unwrap();
    }
    fn expected(&self, user: &UserId) -> Addr {
        let checksum = self
            .app
            .wrap()
            .query_wasm_code_info(self.proxy_code)
            .unwrap()
            .checksum;
        let creator = self
            .app
            .api()
            .addr_canonicalize(self.registry.as_str())
            .unwrap();
        let canonical =
            instantiate2_address(checksum.as_slice(), &creator, &user.proxy_salt().unwrap())
                .unwrap();
        self.app.api().addr_humanize(&canonical).unwrap()
    }
    fn mapping(&self, user: &UserId) -> StdResult<ProxyAddressResponse> {
        self.app.wrap().query_wasm_smart(
            &self.registry,
            &QueryMsg::ProxyAddress {
                user_id: user.clone(),
            },
        )
    }
    fn register(&mut self, user: &UserId) {
        self.app
            .execute_contract(
                self.api.clone(),
                self.registry.clone(),
                &ExecuteMsg::RegisterUser {
                    user_id: user.clone(),
                },
                &[],
            )
            .unwrap();
    }
}

#[test]
fn factory_creates_unique_immutable_proxies_and_rejects_address_substitution() {
    let mut s = Setup::new(false);
    s.configure();
    let alice = UserId::new_email_address("alice@example.com");
    let bob = UserId::new_email_address("bob@example.com");
    assert!(s
        .app
        .execute_contract(
            s.owner.clone(),
            s.registry.clone(),
            &ExecuteMsg::RegisterUser {
                user_id: alice.clone()
            },
            &[]
        )
        .is_err());
    s.register(&alice);
    s.register(&bob);
    assert_eq!(s.mapping(&alice).unwrap().address, s.expected(&alice));
    assert_eq!(s.mapping(&bob).unwrap().address, s.expected(&bob));
    assert_ne!(s.expected(&alice), s.expected(&bob));
    let info = s
        .app
        .wrap()
        .query_wasm_contract_info(s.expected(&alice))
        .unwrap();
    assert_eq!(info.creator, s.registry);
    assert_eq!(info.code_id, s.proxy_code);
    assert!(info.admin.is_none());
    let config: hydro_proxy::msg::ConfigResponse = s
        .app
        .wrap()
        .query_wasm_smart(s.expected(&alice), &hydro_proxy::msg::QueryMsg::Config {})
        .unwrap();
    assert_eq!(config.config.admins, vec![s.handler.clone()]);
    assert_eq!(config.config.control_center_registry, s.centers);
    let victim = UserId::new_email_address("victim@example.com");
    let poisoned = serde_json::json!({"register_user": {"user_id": victim, "proxy_address": s.expected(&alice)}});
    assert!(s
        .app
        .execute_contract(s.api.clone(), s.registry.clone(), &poisoned, &[])
        .is_err());
    assert!(s.mapping(&victim).is_err());
    assert!(s
        .app
        .execute_contract(
            s.api.clone(),
            s.registry.clone(),
            &ExecuteMsg::RegisterUser {
                user_id: alice.clone()
            },
            &[]
        )
        .is_err());
    assert_eq!(s.mapping(&alice).unwrap().address, s.expected(&alice));
}

#[test]
fn only_owner_can_configure_once_and_rotate_provisioners() {
    let mut s = Setup::new(false);
    let user = UserId::new_email_address("alice@example.com");
    assert!(s
        .app
        .execute_contract(
            s.api.clone(),
            s.registry.clone(),
            &ExecuteMsg::RegisterUser {
                user_id: user.clone()
            },
            &[]
        )
        .is_err());
    assert!(s
        .app
        .execute_contract(s.api.clone(), s.registry.clone(), &s.configuration(), &[])
        .is_err());
    s.configure();
    assert!(s
        .app
        .execute_contract(s.owner.clone(), s.registry.clone(), &s.configuration(), &[])
        .is_err());
    s.register(&user);
    let expected = s.expected(&user);
    let replacement = s.app.api().addr_make("replacement-backend");
    let rotate = ExecuteMsg::UpdateAdmins {
        admins: vec![replacement.to_string()],
    };
    assert!(s
        .app
        .execute_contract(s.api.clone(), s.registry.clone(), &rotate, &[])
        .is_err());
    assert!(s
        .app
        .execute_contract(
            s.owner.clone(),
            s.registry.clone(),
            &ExecuteMsg::UpdateAdmins { admins: vec![] },
            &[]
        )
        .is_err());
    s.app
        .execute_contract(s.owner.clone(), s.registry.clone(), &rotate, &[])
        .unwrap();
    let next = UserId::new_email_address("bob@example.com");
    assert!(s
        .app
        .execute_contract(
            s.api.clone(),
            s.registry.clone(),
            &ExecuteMsg::RegisterUser {
                user_id: next.clone()
            },
            &[]
        )
        .is_err());
    s.api = replacement;
    s.register(&next);
    assert_eq!(s.mapping(&user).unwrap().address, expected);
    assert_eq!(s.mapping(&next).unwrap().address, s.expected(&next));
    let config: ConfigResponse = s
        .app
        .wrap()
        .query_wasm_smart(&s.registry, &QueryMsg::Config {})
        .unwrap();
    assert_eq!(config.admins, vec![s.api.clone()]);
    let new_owner = s.app.api().addr_make("new-dao");
    let transfer = ExecuteMsg::TransferOwnership {
        owner: new_owner.to_string(),
    };
    assert!(s
        .app
        .execute_contract(s.api.clone(), s.registry.clone(), &transfer, &[])
        .is_err());
    s.app
        .execute_contract(s.owner.clone(), s.registry.clone(), &transfer, &[])
        .unwrap();
    assert!(s
        .app
        .execute_contract(s.owner.clone(), s.registry.clone(), &rotate, &[])
        .is_err());
    s.app
        .execute_contract(new_owner, s.registry, &rotate, &[])
        .unwrap();
}

#[test]
fn failed_proxy_creation_rolls_back_the_mapping() {
    let mut s = Setup::new(true);
    s.configure();
    let user = UserId::new_email_address("alice@example.com");
    assert!(s
        .app
        .execute_contract(
            s.api.clone(),
            s.registry.clone(),
            &ExecuteMsg::RegisterUser {
                user_id: user.clone()
            },
            &[]
        )
        .is_err());
    assert!(s.mapping(&user).is_err());
    assert!(s
        .app
        .wrap()
        .query_wasm_contract_info(s.expected(&user))
        .is_err());
}

#[test]
fn rejects_noncanonical_ids_and_reuses_only_a_conforming_factory_proxy() {
    let mut s = Setup::new(false);
    s.configure();
    for id in [
        "invalid".to_owned(),
        "a".repeat(62),
        "A".repeat(64),
        "g".repeat(64),
    ] {
        assert!(s
            .app
            .execute_contract(
                s.api.clone(),
                s.registry.clone(),
                &ExecuteMsg::RegisterUser {
                    user_id: UserId::new_raw(id)
                },
                &[]
            )
            .is_err());
    }
    let user = UserId::new_email_address("alice@example.com");
    // Simulate an existing factory-created proxy without a mapping.
    let address = s
        .app
        .instantiate2_contract(
            s.proxy_code,
            s.registry.clone(),
            &hydro_proxy::msg::InstantiateMsg {
                admins: vec![s.handler.to_string()],
                control_center_registry: s.centers.to_string(),
            },
            &[],
            "existing",
            None,
            user.proxy_salt().unwrap(),
        )
        .unwrap();
    s.register(&user);
    assert_eq!(s.mapping(&user).unwrap().address, address);
    let user = UserId::new_email_address("bob@example.com");
    s.app
        .instantiate2_contract(
            s.proxy_code,
            s.registry.clone(),
            &hydro_proxy::msg::InstantiateMsg {
                admins: vec![s.api.to_string()],
                control_center_registry: s.centers.to_string(),
            },
            &[],
            "invalid",
            None,
            user.proxy_salt().unwrap(),
        )
        .unwrap();
    assert!(s
        .app
        .execute_contract(
            s.api.clone(),
            s.registry.clone(),
            &ExecuteMsg::RegisterUser {
                user_id: user.clone()
            },
            &[]
        )
        .is_err());
    assert!(s.mapping(&user).is_err());
}
