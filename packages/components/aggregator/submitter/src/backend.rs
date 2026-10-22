pub(crate) use app_component_backend_client::BackendClient;

use crate::reporting::Backend;
use app_component_backend_client::ActionRecord;
use serde_json::Value;

impl Backend for BackendClient {
    async fn create_action(&self, body: Value) -> anyhow::Result<ActionRecord> {
        BackendClient::create_action(self, body).await
    }

    async fn report_submission(&self, id: i64) -> anyhow::Result<ActionRecord> {
        BackendClient::report_submission(self, id).await
    }

    async fn report_result(&self, id: i64, body: Value) -> anyhow::Result<ActionRecord> {
        BackendClient::report_result(self, id, body).await
    }
}
