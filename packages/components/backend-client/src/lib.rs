use anyhow::{ensure, Result};
use app_contract_api::service_handler::msg::CustomExecuteMsg;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use wstd::http::{Body, BodyExt, Client, Request, Uri};

#[derive(Debug, Clone, Deserialize)]
pub struct ActionRecord {
    pub id: i64,
    pub chain_key: String,
    pub service_handler_address: String,
    pub event_id: String,
    pub payload: CustomExecuteMsg,
    pub status: String,
}

pub struct BackendClient {
    base_url: String,
    api_key: String,
}

impl BackendClient {
    pub fn from_env() -> Result<Self> {
        let base_url = std::env::var("WAVS_ENV_BACKEND_API_URL")
            .map_err(|_| anyhow::anyhow!("WAVS_ENV_BACKEND_API_URL is required"))?;
        let api_key = std::env::var("WAVS_ENV_BACKEND_API_KEY")
            .map_err(|_| anyhow::anyhow!("WAVS_ENV_BACKEND_API_KEY is required"))?;
        Self::new(base_url, api_key)
    }

    fn new(base_url: String, api_key: String) -> Result<Self> {
        let uri: Uri = base_url
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid backend API URL"))?;
        ensure!(
            matches!(uri.scheme_str(), Some("http" | "https"))
                && uri.host().is_some()
                && uri.query().is_none()
                && !base_url.contains('@')
                && !base_url.contains('#'),
            "Invalid backend API URL"
        );
        ensure!(
            api_key.len() >= 32 && api_key.bytes().all(|v| v.is_ascii_graphic()),
            "Invalid backend API key"
        );
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
        })
    }

    fn request(&self, path: &str, body: Option<Value>) -> Result<Request<Body>> {
        Request::post(format!("{}{path}", self.base_url))
            .header("authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
            .map_err(|_| anyhow::anyhow!("Could not construct backend request"))
    }

    pub async fn post(&self, path: &str, body: Option<Value>) -> Result<ActionRecord> {
        let mut client = Client::new();
        client.set_connect_timeout(Duration::from_secs(5));
        client.set_first_byte_timeout(Duration::from_secs(10));
        client.set_between_bytes_timeout(Duration::from_secs(5));
        let request = self.request(path, body)?;
        // Do not include request headers, response bodies, or transport errors in logs.
        let response = client
            .send(request)
            .await
            .map_err(|_| anyhow::anyhow!("Backend API request failed"))?;
        ensure!(
            matches!(response.status().as_u16(), 200 | 201),
            "Backend API returned HTTP {}",
            response.status().as_u16()
        );
        let mut bytes = Vec::new();
        let mut body = response.into_body().into_boxed_body();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| anyhow::anyhow!("Could not read backend response"))?;
            if let Ok(data) = frame.into_data() {
                ensure!(
                    bytes.len() + data.len() <= 65_536,
                    "Backend response exceeded size limit"
                );
                bytes.extend_from_slice(&data);
            }
        }
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("Invalid backend response"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use serde_json::json;

    const KEY: &str = "test-backend-credential-32-characters";

    #[test]
    fn builds_authenticated_json_requests_and_empty_submission_bodies() {
        let client = BackendClient::new("http://127.0.0.1:8080/".into(), KEY.into()).unwrap();
        let value = json!({"event_id": "01".repeat(20), "payload": {"email": {"from": "02".repeat(32), "subject": "deposit"}}});
        let mut request = client.request("/v1/actions", Some(value.clone())).unwrap();
        assert_eq!(request.method(), "POST");
        assert_eq!(request.uri(), "http://127.0.0.1:8080/v1/actions");
        assert_eq!(request.headers()["authorization"], format!("Bearer {KEY}"));
        assert_eq!(request.headers()["content-type"], "application/json");
        assert_eq!(
            serde_json::from_slice::<Value>(
                block_on(async move { request.body_mut().contents().await.unwrap().to_vec() })
                    .as_slice()
            )
            .unwrap(),
            value
        );
        let mut request = client.request("/v1/actions/42/submission", None).unwrap();
        assert!(block_on(async move {
            request.body_mut().contents().await.unwrap().is_empty()
        }));
    }

    #[test]
    fn rejects_invalid_configuration_without_echoing_secrets() {
        for url in [
            "",
            "ftp://example.com",
            "http://user:password@example.com",
            "http://example.com?key=secret",
            "http://example.com/#fragment",
        ] {
            let err = BackendClient::new(url.into(), KEY.into()).err().unwrap();
            assert_eq!(err.to_string(), "Invalid backend API URL");
        }
        for key in ["", "short", "test-backend-credential-with-newline\n"] {
            let err = BackendClient::new("https://example.com".into(), key.into())
                .err()
                .unwrap();
            assert_eq!(err.to_string(), "Invalid backend API key");
        }
    }
}
