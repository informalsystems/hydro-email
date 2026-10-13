#![recursion_limit = "256"]

use app_tests_common::shared_tests::integration::test_integration;
use app_utils::tracing::tracing_init;
use off_chain_tests::client::{
    proxy::ProxyClient, service_handler::ServiceHandlerClient, user_registry::UserRegistryClient,
    AppClient,
};

#[tokio::test]
async fn integration() {
    tracing_init();

    let app_client = AppClient::new("admin");
    let user_registry = UserRegistryClient::new(app_client.clone());
    let service_handler = ServiceHandlerClient::new(app_client.clone(), user_registry.address);

    let proxy_code_id = ProxyClient::code_id(&app_client);

    let proxy = ProxyClient::new(
        app_client.clone(),
        proxy_code_id,
        vec![service_handler.address.clone()],
    );

    let wrong_proxy = ProxyClient::new(
        app_client.clone(),
        proxy_code_id,
        vec![service_handler.address.clone()],
    );

    test_integration(service_handler, proxy, wrong_proxy).await;
}
