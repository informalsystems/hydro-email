use app_client::executor::SigningClientWrapper;
use app_contract_api::{service_handler::msg::UserIdEmail, user_registry::msg::UserId};
use app_tests_common::shared_tests::integration::test_integration;
use app_utils::tracing::{env_init, tracing_init};
use cosmwasm_std::Uint128;
use hydro_proxy::state::ActionState;
use on_chain_tests::client::{
    control_center::ControlCenterClient, proxy::ProxyClient, service_handler::ServiceHandlerClient,
    user_registry::UserRegistryClient, vault::VaultClient, AppClient,
};

use std::sync::Arc;

async fn setup() -> TestSetup {
    tracing_init();
    env_init();

    let app_client = AppClient::new().await;
    let client: SigningClientWrapper = Arc::new(app_client.pool().get().await.unwrap());

    let control_center = ControlCenterClient::new(
        client.clone(),
        vec![client.addr.clone()],
        Uint128::new(1_000_000_000_000),
    )
    .await;

    let vault = VaultClient::new(
        client.clone(),
        control_center.address.clone(),
        "untrn".to_string(),
        vec![client.addr.clone()],
    )
    .await;

    control_center
        .add_subvault(&client, vault.address.clone())
        .await;

    let user_registry = UserRegistryClient::new(client.clone(), None).await;
    let service_handler =
        ServiceHandlerClient::new(client.clone(), user_registry.address.clone(), None).await;

    let proxy = ProxyClient::new(
        client.clone(),
        Some(vec![service_handler.address.clone()]),
        vec![control_center.address.clone()],
    )
    .await;
    let wrong_proxy = ProxyClient::new(
        client.clone(),
        Some(vec![service_handler.address.clone()]),
        vec![control_center.address.clone()],
    )
    .await;

    TestSetup {
        client,
        control_center,
        vault,
        user_registry,
        service_handler,
        proxy,
        wrong_proxy,
    }
}

struct TestSetup {
    client: SigningClientWrapper,
    #[allow(dead_code)]
    control_center: ControlCenterClient,
    vault: VaultClient,
    user_registry: UserRegistryClient,
    service_handler: ServiceHandlerClient,
    proxy: ProxyClient,
    wrong_proxy: ProxyClient,
}

#[tokio::test]
async fn integration() {
    let setup = setup().await;

    test_integration(
        setup.service_handler.clone(),
        setup.proxy.clone(),
        setup.wrong_proxy.clone(),
        on_chain_tests::code_ids::CodeId::new_proxy().await,
    )
    .await;
}

#[tokio::test]
async fn deposit_and_withdraw() {
    let mut setup = setup().await;

    let email = UserIdEmail {
        from: UserId::new_email_address("depositor@example.com"),
        subject: "deposit".to_string(),
    };

    let config = setup.proxy.querier.config().await.unwrap().config;
    setup
        .user_registry
        .executor
        .exec(
            &app_contract_api::user_registry::msg::ExecuteMsg::ConfigureProxy {
                proxy_code_id: on_chain_tests::code_ids::CodeId::new_proxy().await,
                service_handler: setup.service_handler.address.to_string(),
                control_center_registry: config.control_center_registry.to_string(),
            },
            &[],
        )
        .await
        .unwrap();
    // Register user's proxy address
    setup
        .user_registry
        .executor
        .register_user_id(email.from.clone())
        .await
        .unwrap();

    let address = setup
        .user_registry
        .querier
        .proxy_address_user_id(email.from.clone())
        .await
        .unwrap();
    setup.proxy.address = setup
        .client
        .querier
        .chain_config
        .parse_address(&address.to_string())
        .unwrap();
    setup.proxy.querier.addr = address.clone();
    setup.proxy.executor.addr = address;

    // Fund the proxy with some untrn for deposit
    let deposit_amount = 1_000_000u128;
    setup
        .client
        .transfer(deposit_amount, &setup.proxy.address, "untrn", None)
        .await
        .unwrap();

    // Verify proxy received funds
    let proxy_balance = setup
        .client
        .querier
        .balance(setup.proxy.address.clone(), Some("untrn".to_string()))
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(proxy_balance, deposit_amount);

    // Send deposit email
    setup
        .service_handler
        .executor
        .push_email(email.clone())
        .await
        .unwrap();

    // Verify proxy state changed to Forwarded (deposit)
    let state = setup.proxy.querier.state().await.unwrap();
    assert_eq!(state.last_action, ActionState::Forwarded);

    // Verify proxy received vault shares
    let shares_denom = setup.vault.shares_denom().await;
    let proxy_shares = setup
        .client
        .querier
        .balance(setup.proxy.address.clone(), Some(shares_denom.clone()))
        .await
        .unwrap()
        .unwrap_or(0);
    assert!(
        proxy_shares > 0,
        "proxy should have vault shares after deposit"
    );

    // Verify proxy untrn balance is now 0
    let proxy_balance_after = setup
        .client
        .querier
        .balance(setup.proxy.address.clone(), Some("untrn".to_string()))
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(
        proxy_balance_after, 0,
        "proxy should have sent all untrn to vault"
    );

    // Now test withdrawal - send email with withdraw command
    let recipient_addr = setup.client.addr.to_string();
    let withdraw_email = UserIdEmail {
        from: UserId::new_email_address("depositor@example.com"),
        subject: format!(
            "withdraw {} {} {}",
            recipient_addr, shares_denom, proxy_shares
        ),
    };

    setup
        .service_handler
        .executor
        .push_email(withdraw_email)
        .await
        .unwrap();

    // Verify proxy state changed to WithdrawFunds
    let state = setup.proxy.querier.state().await.unwrap();
    match state.last_action {
        ActionState::WithdrawFunds { recipient, coin } => {
            assert_eq!(recipient.to_string(), recipient_addr);
            assert_eq!(coin.denom, shares_denom);
        }
        _ => panic!("expected WithdrawFunds state, got {:?}", state.last_action),
    }
}
