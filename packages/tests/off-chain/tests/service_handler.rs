#![recursion_limit = "256"]

use app_utils::tracing::tracing_init;
use off_chain_tests::client::service_handler::ServiceHandlerClient;
use off_chain_tests::client::user_registry::UserRegistryClient;
use off_chain_tests::client::AppClient;

#[tokio::test]
async fn get_admin() {
    tracing_init();

    let app_client = AppClient::new("admin");
    let user_registry = UserRegistryClient::new(app_client.clone());
    let service_handler = ServiceHandlerClient::new(app_client.clone(), user_registry.address);

    let admin = app_client.admin().to_string();

    app_tests_common::shared_tests::service_handler::get_admin(&service_handler.querier, &admin)
        .await;
}
