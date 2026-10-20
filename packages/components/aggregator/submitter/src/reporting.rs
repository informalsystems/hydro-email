use anyhow::{bail, ensure, Result};
use app_contract_api::service_handler::msg::CustomExecuteMsg;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ActionRecord {
    pub id: i64,
    pub chain_key: String,
    pub service_handler_address: String,
    pub event_id: String,
    pub payload: CustomExecuteMsg,
    pub status: String,
}

pub(crate) trait Backend {
    async fn post(&self, path: &str, body: Option<Value>) -> Result<ActionRecord>;
}

pub(crate) struct ActionReport {
    pub chain: String,
    pub handler: String,
    pub event_id: String,
    pub payload: CustomExecuteMsg,
}

impl ActionReport {
    pub fn new(chain: String, handler: String, event_id: &[u8], payload: &[u8]) -> Result<Self> {
        ensure!(event_id.len() == 20, "Invalid WAVS event ID");
        let payload = CustomExecuteMsg::decode(payload)
            .map_err(|_| anyhow::anyhow!("Invalid WAVS action payload"))?;
        Ok(Self {
            chain,
            handler,
            event_id: const_hex::encode(event_id),
            payload,
        })
    }

    async fn create(&self, backend: &impl Backend) -> Result<ActionRecord> {
        let action = backend
            .post(
                "/v1/actions",
                Some(json!({
                    "event_id": self.event_id,
                    "payload": self.payload,
                })),
            )
            .await?;
        self.validate(&action)?;
        Ok(action)
    }

    fn validate(&self, action: &ActionRecord) -> Result<()> {
        ensure!(
            action.id > 0
                && action.chain_key == self.chain
                && action.service_handler_address == self.handler
                && action.event_id == self.event_id
                && action.payload == self.payload,
            "Backend action does not match the WAVS event and deployment"
        );
        Ok(())
    }

    pub async fn prepare(&self, backend: &impl Backend) -> Result<bool> {
        let action = self.create(backend).await?;
        match action.status.as_str() {
            "received" => {
                let updated = backend
                    .post(&format!("/v1/actions/{}/submission", action.id), None)
                    .await?;
                self.validate(&updated)?;
                ensure!(
                    updated.id == action.id && updated.status == "submitting",
                    "Backend did not record submission intent"
                );
                Ok(true)
            }
            // Other operators' signatures for this event must still reach WAVS's quorum queue.
            "submitting" => Ok(true),
            "completed" | "failed" | "outcome_unknown" => Ok(false),
            _ => bail!("Unexpected backend action status"),
        }
    }

    pub async fn callback(&self, backend: &impl Backend, outcome: CallbackOutcome) -> Result<()> {
        let Some(result) = outcome.report() else {
            return Ok(());
        };
        // Reuse the idempotent create endpoint to recover the action ID after a restart.
        let action = self.create(backend).await?;
        if action.status == "received" {
            let updated = backend
                .post(&format!("/v1/actions/{}/submission", action.id), None)
                .await?;
            self.validate(&updated)?;
            ensure!(
                updated.id == action.id && updated.status == "submitting",
                "Backend did not record submission intent"
            );
        }
        let updated = backend
            .post(
                &format!("/v1/actions/{}/result", action.id),
                Some(result.clone()),
            )
            .await?;
        self.validate(&updated)?;
        ensure!(
            updated.id == action.id && Some(updated.status.as_str()) == result["status"].as_str(),
            "Backend did not record the callback outcome"
        );
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum CallbackOutcome {
    Completed(String),
    Failed(String),
    Unknown,
    WaitingForQuorum,
}

impl CallbackOutcome {
    pub fn cosmos(result: Result<String, String>) -> Self {
        match result {
            Ok(hash) if hash.len() == 64 && hash.bytes().all(|v| v.is_ascii_hexdigit()) => {
                Self::Completed(hash.to_ascii_uppercase())
            }
            // The pinned runtime calls back on insufficient quorum before broadcasting.
            Err(error) if error.starts_with("Insufficient Quorum:") => Self::WaitingForQuorum,
            // Another aggregator may have executed the event; a replay failure is not action failure.
            Err(error) if error.contains("Event already processed") => Self::Unknown,
            // Only explicit CheckTx/DeliverTx failures establish execution failure.
            Err(error) => chain_failure_detail(&error)
                .map(Self::Failed)
                .unwrap_or(Self::Unknown),
            Ok(_) => Self::Unknown,
        }
    }

    fn report(self) -> Option<Value> {
        match self {
            Self::Completed(hash) => Some(json!({"status": "completed", "tx_hash": hash})),
            Self::Failed(detail) => Some(json!({
                "status": "failed", "error_code": "transaction_failed", "error_detail": detail,
            })),
            Self::Unknown => Some(
                json!({"status": "outcome_unknown", "error_code": "submission_outcome_unknown",
                    "error_detail": "Submission confirmation was unavailable or unrecognized; execution may have occurred."}),
            ),
            Self::WaitingForQuorum => None,
        }
    }
}

// Extract bounded chain metadata instead of copying raw logs, which may contain secrets.
fn chain_failure_detail(error: &str) -> Option<String> {
    let tail = error.strip_prefix("Cosmos Client: tx failed with code: ")?;
    let code: u32 = tail.split(',').next()?.trim().parse().ok()?;
    if code == 0 {
        return None;
    }
    let codespace = tail.split(',').find_map(|field| {
        let value = field.trim().strip_prefix("codespace: ")?;
        (!value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || v == b'_'))
        .then_some(value)
    });
    Some(match codespace {
        Some(codespace) => {
            format!("Transaction rejected with code {code} in codespace {codespace}.")
        }
        None => format!("Transaction rejected with code {code}."),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_contract_api::{service_handler::msg::UserIdEmail, user_registry::msg::UserId};
    use futures::executor::block_on;
    use std::{cell::RefCell, collections::VecDeque};

    #[derive(Default)]
    struct MockBackend {
        replies: RefCell<VecDeque<Result<ActionRecord>>>,
        requests: RefCell<Vec<(String, Option<Value>)>>,
    }

    impl MockBackend {
        fn new(replies: Vec<Result<ActionRecord>>) -> Self {
            Self {
                replies: RefCell::new(replies.into()),
                ..Default::default()
            }
        }
    }

    impl Backend for MockBackend {
        async fn post(&self, path: &str, body: Option<Value>) -> Result<ActionRecord> {
            self.requests.borrow_mut().push((path.into(), body));
            self.replies
                .borrow_mut()
                .pop_front()
                .expect("Unexpected API request")
        }
    }

    fn report() -> ActionReport {
        let payload = CustomExecuteMsg::Email(UserIdEmail {
            from: UserId::new_raw("02".repeat(32)),
            subject: "deposit".into(),
        });
        ActionReport::new(
            "dev:local".into(),
            "handler".into(),
            &[1; 20],
            &payload.encode().unwrap(),
        )
        .unwrap()
    }

    fn record(status: &str) -> ActionRecord {
        let report = report();
        ActionRecord {
            id: 42,
            chain_key: report.chain,
            service_handler_address: report.handler,
            event_id: report.event_id,
            payload: report.payload,
            status: status.into(),
        }
    }

    #[test]
    fn records_intent_before_allowing_submission_and_preserves_quorum_collection() {
        let report = report();
        let backend = MockBackend::new(vec![Ok(record("received")), Ok(record("submitting"))]);
        assert!(block_on(report.prepare(&backend)).unwrap());
        assert_eq!(
            *backend.requests.borrow(),
            [
                (
                    "/v1/actions".into(),
                    Some(json!({"event_id": "01".repeat(20), "payload": report.payload}))
                ),
                ("/v1/actions/42/submission".into(), None),
            ]
        );
        // A later signature must still be passed to WAVS for the same in-flight event.
        let backend = MockBackend::new(vec![Ok(record("submitting"))]);
        assert!(block_on(report.prepare(&backend)).unwrap());
        assert_eq!(backend.requests.borrow().len(), 1);
    }

    #[test]
    fn terminal_and_uncertain_actions_do_not_request_another_submission() {
        for status in ["completed", "failed", "outcome_unknown"] {
            let backend = MockBackend::new(vec![Ok(record(status))]);
            assert!(!block_on(report().prepare(&backend)).unwrap());
            assert_eq!(backend.requests.borrow().len(), 1);
        }
    }

    #[test]
    fn api_failure_before_submission_fails_closed_without_retrying() {
        for (replies, expected_requests) in [
            (vec![Err(anyhow::anyhow!("API unavailable"))], 1),
            (
                vec![
                    Ok(record("received")),
                    Err(anyhow::anyhow!("API unavailable")),
                ],
                2,
            ),
        ] {
            let backend = MockBackend::new(replies);
            assert!(block_on(report().prepare(&backend)).is_err());
            assert_eq!(backend.requests.borrow().len(), expected_requests);
        }
    }

    #[test]
    fn mismatched_backend_records_never_authorize_submission() {
        let mut records = Vec::new();
        let mut action = record("received");
        action.chain_key = "other-chain".into();
        records.push(action);
        let mut action = record("received");
        action.service_handler_address = "other-handler".into();
        records.push(action);
        let mut action = record("received");
        action.event_id = "03".repeat(20);
        records.push(action);
        let mut action = record("received");
        action.id = 0;
        records.push(action);
        let mut action = record("received");
        let CustomExecuteMsg::Email(email) = &mut action.payload;
        email.subject = "forward".into();
        records.push(action);
        for action in records {
            let backend = MockBackend::new(vec![Ok(action)]);
            assert!(block_on(report().prepare(&backend)).is_err());
            assert_eq!(backend.requests.borrow().len(), 1);
        }
        let mut wrong_id = record("submitting");
        wrong_id.id = 43;
        let backend = MockBackend::new(vec![Ok(record("received")), Ok(wrong_id)]);
        assert!(block_on(report().prepare(&backend)).is_err());
    }

    #[test]
    fn callback_reports_confirmed_success_or_conservative_failure_categories() {
        for (result, expected) in [
            (
                Ok("ab".repeat(32)),
                json!({"status": "completed", "tx_hash": "AB".repeat(32)}),
            ),
            (
                Err(
                    "Cosmos Client: tx failed with code: 5, codespace: sdk, raw_log: failed".into(),
                ),
                json!({"status": "failed", "error_code": "transaction_failed",
                    "error_detail": "Transaction rejected with code 5 in codespace sdk."}),
            ),
            (
                Err("Cosmos Client: timeout waiting for confirmation".into()),
                json!({"status": "outcome_unknown", "error_code": "submission_outcome_unknown",
                    "error_detail": "Submission confirmation was unavailable or unrecognized; execution may have occurred."}),
            ),
        ] {
            let status = expected["status"].as_str().unwrap();
            let backend = MockBackend::new(vec![Ok(record("submitting")), Ok(record(status))]);
            block_on(report().callback(&backend, CallbackOutcome::cosmos(result))).unwrap();
            assert_eq!(
                *backend.requests.borrow(),
                [
                    (
                        "/v1/actions".into(),
                        Some(json!({"event_id": "01".repeat(20), "payload": report().payload}))
                    ),
                    ("/v1/actions/42/result".into(), Some(expected)),
                ]
            );
        }
    }

    #[test]
    fn failure_details_extract_only_chain_metadata() {
        for raw in [
            "Cosmos Client: tx failed with code: 5, codespace: sdk, raw_log: secret https://rpc.example/?key=secret",
            "Cosmos Client: tx failed with code: 5, codespace: sdk, raw_log: different secret",
        ] {
            let value = CallbackOutcome::cosmos(Err(raw.into())).report().unwrap();
            assert_eq!(value["error_detail"], "Transaction rejected with code 5 in codespace sdk.");
        }
        for codespace in ["https://rpc.example/?key=secret".to_owned(), "x".repeat(65)] {
            let value = CallbackOutcome::cosmos(Err(format!(
                "Cosmos Client: tx failed with code: 5, codespace: {codespace}, raw_log: secret"
            )))
            .report()
            .unwrap();
            assert_eq!(value["error_detail"], "Transaction rejected with code 5.");
        }
        let value = CallbackOutcome::cosmos(Err("RPC failed at https://secret@example.com".into()))
            .report()
            .unwrap();
        assert!(!value.to_string().contains("secret"));
    }

    #[test]
    fn insufficient_quorum_does_not_record_a_terminal_failure() {
        let outcome = CallbackOutcome::cosmos(Err(
            "Insufficient Quorum: weight 1 < threshold 2 (total weight: 2)".into(),
        ));
        assert_eq!(outcome, CallbackOutcome::WaitingForQuorum);
        let backend = MockBackend::default();
        block_on(report().callback(&backend, outcome)).unwrap();
        assert!(backend.requests.borrow().is_empty());
    }

    #[test]
    fn callback_api_failure_never_requests_another_submission() {
        let backend = MockBackend::new(vec![
            Ok(record("submitting")),
            Err(anyhow::anyhow!("API unavailable")),
        ]);
        assert!(
            block_on(report().callback(&backend, CallbackOutcome::Completed("AB".repeat(32))))
                .is_err()
        );
        let requests = backend.requests.borrow();
        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .all(|(path, _)| !path.ends_with("/submission")));
    }

    #[test]
    fn callback_recovers_the_record_without_a_persisted_local_action_id() {
        let backend = MockBackend::new(vec![
            Ok(record("received")),
            Ok(record("submitting")),
            Ok(record("completed")),
        ]);
        block_on(report().callback(&backend, CallbackOutcome::Completed("AB".repeat(32)))).unwrap();
        assert_eq!(
            backend
                .requests
                .borrow()
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            [
                "/v1/actions",
                "/v1/actions/42/submission",
                "/v1/actions/42/result"
            ]
        );
        let backend = MockBackend::new(vec![Ok(record("completed")), Ok(record("completed"))]);
        block_on(report().callback(&backend, CallbackOutcome::Completed("AB".repeat(32)))).unwrap();
        assert_eq!(backend.requests.borrow().len(), 2);
    }

    #[test]
    fn malformed_callback_hashes_and_unrecognized_errors_remain_unknown() {
        for result in [
            Ok("short".into()),
            Ok("gg".repeat(32)),
            Err("Cosmos Client: tx failed with code: 0, codespace: sdk".into()),
            Err("Cosmos Client: tx failed with code: invalid".into()),
            Err("unexpected error".into()),
            Err("Cosmos Client: tx failed with code: 5, raw_log: Event already processed".into()),
        ] {
            assert_eq!(CallbackOutcome::cosmos(result), CallbackOutcome::Unknown);
        }
        assert!(ActionReport::new("dev:local".into(), "handler".into(), &[1; 19], b"{}").is_err());
        assert!(
            ActionReport::new("dev:local".into(), "handler".into(), &[1; 20], b"invalid").is_err()
        );
    }
}
