pub(crate) use app_component_backend_client::BackendClient;

use crate::reporting::Backend;
use app_component_backend_client::ActionRecord;

impl Backend for BackendClient {
    async fn post(
        &self,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> anyhow::Result<ActionRecord> {
        BackendClient::post(self, path, body).await
    }
}
