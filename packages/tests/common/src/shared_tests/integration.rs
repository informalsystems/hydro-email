use app_client::contracts::{
    proxy::ProxyContract, service_handler::ServiceHandlerContract,
    user_registry::UserRegistryContract,
};
use app_contract_api::{service_handler::msg::UserIdEmail, user_registry::msg::UserId};
use hydro_proxy::state::{ActionState, State};

pub async fn test_integration(
    service_handler: impl Into<ServiceHandlerContract>,
    proxy: impl Into<ProxyContract>,
    wrong_proxy: impl Into<ProxyContract>,
) {
    let service_handler = service_handler.into();
    let proxy = proxy.into();
    let wrong_proxy = wrong_proxy.into();

    let user_registry = UserRegistryContract::new(
        service_handler.querier.inner.clone(),
        service_handler.executor.inner.clone(),
        service_handler
            .querier
            .user_registry_address()
            .await
            .unwrap(),
    );

    let email = UserIdEmail {
        from: UserId::new_email_address("alice@example.com"),
        subject: "deposit".to_string(),
    };

    // An email from a different user to verify we get the right proxy address
    let not_this_email = UserIdEmail {
        from: UserId::new_email_address("bob@example.com"),
        subject: "deposit".to_string(),
    };

    // no proxy address yet
    user_registry
        .querier
        .proxy_address_user_id(email.from.clone())
        .await
        .unwrap_err();

    // register the user's proxy address
    user_registry
        .executor
        .register_user_id(email.from.clone(), proxy.address.clone())
        .await
        .unwrap();

    // register a different user's proxy address to verify we get the right one
    user_registry
        .executor
        .register_user_id(not_this_email.from.clone(), wrong_proxy.address.clone())
        .await
        .unwrap();

    // got the right one
    assert_eq!(
        user_registry
            .querier
            .proxy_address_user_id(email.from.clone())
            .await
            .unwrap(),
        proxy.address
    );

    // Proxy is idle at first
    let state = proxy.querier.state().await.unwrap();

    assert_eq!(
        state,
        State {
            last_action: ActionState::Idle
        }
    );

    // Send an email through the service handler
    service_handler
        .executor
        .push_email(email.clone())
        .await
        .unwrap();

    // Service handler should get the right proxy address and update the state
    let state = proxy.querier.state().await.unwrap();
    assert_eq!(state.last_action, ActionState::Forwarded);

    // wrong proxy is still idle
    let state = wrong_proxy.querier.state().await.unwrap();
    assert_eq!(state.last_action, ActionState::Idle);
}
