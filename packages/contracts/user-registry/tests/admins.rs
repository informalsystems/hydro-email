use cw_multi_test::{App, ContractWrapper, Executor};

#[test]
fn dedicated_api_admin_can_register_users_but_deployer_cannot() {
    use app_contract_api::user_registry::msg::{
        ExecuteMsg as UserExecute, InstantiateMsg as UserInstantiate, ProxyAddressResponse,
        QueryMsg as UserQuery, UserId,
    };
    let mut app = App::default();
    let deployer = app.api().addr_make("deployment-wallet");
    let api = app.api().addr_make("api-wallet");
    let proxy = app.api().addr_make("user-proxy");
    let code = app.store_code(Box::new(ContractWrapper::new(
        app_contract_user_registry::execute,
        app_contract_user_registry::instantiate,
        app_contract_user_registry::query,
    )));
    let registry = app
        .instantiate_contract(
            code,
            deployer.clone(),
            &UserInstantiate {
                admins: vec![api.to_string()],
            },
            &[],
            "users",
            None,
        )
        .unwrap();
    let user_id = UserId::new_email_address("alice@example.com");
    let register = UserExecute::RegisterUser {
        user_id: user_id.clone(),
        proxy_address: proxy.to_string(),
    };
    assert!(app
        .execute_contract(deployer, registry.clone(), &register, &[])
        .is_err());
    app.execute_contract(api, registry.clone(), &register, &[])
        .unwrap();
    let mapping: ProxyAddressResponse = app
        .wrap()
        .query_wasm_smart(&registry, &UserQuery::ProxyAddress { user_id })
        .unwrap();
    assert_eq!(mapping.address, proxy);
}
