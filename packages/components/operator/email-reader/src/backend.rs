use anyhow::{ensure, Result};
use app_component_backend_client::{ActionRecord, BackendClient, RejectionRecord};
use app_contract_api::service_handler::msg::CustomExecuteMsg;
use serde_json::{json, Value};

use crate::email::{
    cache::{CacheStore, MessageId, ProcessedEmails},
    parser::InvalidEmail,
};

pub(crate) trait Backend {
    async fn create(&self, request: Value) -> Result<ActionRecord>;
    async fn reject(&self, request: Value) -> Result<RejectionRecord>;
}

impl Backend for BackendClient {
    async fn reject(&self, request: Value) -> Result<RejectionRecord> {
        self.reject_mailbox_message(request).await
    }

    async fn create(&self, request: Value) -> Result<ActionRecord> {
        self.create_action(request).await
    }
}

pub(crate) async fn reject(
    backend: &impl Backend,
    chain: &str,
    handler: &str,
    message: MessageId,
    reason: InvalidEmail,
    cache: &ProcessedEmails<impl CacheStore>,
) -> Result<()> {
    let code = reason.code();
    let saved = backend
        .reject(json!({
            "mailbox_message": {
                "folder": "INBOX", "uid_validity": message.uid_validity, "uid": message.uid,
            },
            "error_code": code,
        }))
        .await?;
    ensure!(
        saved.id > 0
            && saved.action_id.is_none()
            && saved.rejection_chain_key == chain
            && saved.rejection_service_handler_address == handler
            && saved.rejection_code == code
            && saved.folder == "INBOX"
            && saved.uid_validity == message.uid_validity
            && saved.uid == message.uid,
        "Backend rejection does not match the mailbox message and deployment"
    );
    // Never suppress the message until its rejection is safely persisted.
    cache.remember(message)?;
    println!("Email rejected: {code}");
    Ok(())
}

// Register metadata separately so mailbox identifiers never enter the signed payload.
// Returns whether this action still needs an operator response.
pub(crate) async fn register(
    backend: &impl Backend,
    chain: &str,
    handler: &str,
    event_id: &[u8],
    payload: &CustomExecuteMsg,
    message: MessageId,
) -> Result<bool> {
    ensure!(event_id.len() == 20, "Invalid WAVS event ID");
    let event_id = const_hex::encode(event_id);
    let action = backend
        .create(json!({
            "event_id": event_id,
            "payload": payload,
            "mailbox_message": {
                "folder": "INBOX",
                "uid_validity": message.uid_validity,
                "uid": message.uid,
            },
        }))
        .await?;
    ensure!(
        action.id > 0
            && action.chain_key == chain
            && action.service_handler_address == handler
            && action.event_id == event_id
            && action.payload == *payload,
        "Backend action does not match the WAVS event and deployment"
    );
    match action.status.as_str() {
        "received" | "submitting" => Ok(true),
        "completed" | "failed" | "outcome_unknown" => Ok(false),
        _ => anyhow::bail!("Unexpected backend action status"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_contract_api::{service_handler::msg::UserIdEmail, user_registry::msg::UserId};
    use futures::executor::block_on;
    use std::cell::RefCell;

    #[derive(Default)]
    struct MockBackend {
        rejection_reply: Option<RejectionRecord>,
        reply: Option<ActionRecord>,
        requests: RefCell<Vec<Value>>,
    }

    impl Backend for MockBackend {
        async fn reject(&self, _: Value) -> Result<RejectionRecord> {
            self.rejection_reply
                .clone()
                .ok_or_else(|| anyhow::anyhow!("API unavailable"))
        }

        async fn create(&self, request: Value) -> Result<ActionRecord> {
            self.requests.borrow_mut().push(request);
            self.reply
                .clone()
                .ok_or_else(|| anyhow::anyhow!("API unavailable"))
        }
    }

    #[test]
    fn registers_private_identifiers_and_preserves_quorum_and_terminal_states() {
        let payload = CustomExecuteMsg::Email(UserIdEmail {
            from: UserId::new_raw("02".repeat(32)),
            subject: "deposit".into(),
        });
        for (status, expected) in [
            ("received", true),
            ("submitting", true),
            ("completed", false),
            ("failed", false),
            ("outcome_unknown", false),
        ] {
            let backend = MockBackend {
                reply: Some(ActionRecord {
                    id: 1,
                    chain_key: "dev:local".into(),
                    service_handler_address: "handler".into(),
                    event_id: "01".repeat(20),
                    payload: payload.clone(),
                    status: status.into(),
                }),
                requests: RefCell::default(),
                ..Default::default()
            };
            let run = || {
                block_on(register(
                    &backend,
                    "dev:local",
                    "handler",
                    &[1; 20],
                    &payload,
                    MessageId {
                        uid_validity: 100,
                        uid: 10,
                    },
                ))
            };
            assert_eq!(run().unwrap(), expected);
            assert_eq!(
                backend.requests.borrow()[0]["mailbox_message"],
                json!({"folder": "INBOX", "uid_validity": 100, "uid": 10})
            );
            assert!(serde_json::to_value(&payload)
                .unwrap()
                .get("mailbox_message")
                .is_none());
        }
    }

    #[test]
    fn unavailable_or_mismatched_backend_does_not_allow_a_response() {
        let payload = CustomExecuteMsg::Email(UserIdEmail {
            from: UserId::new_raw("02".repeat(32)),
            subject: "deposit".into(),
        });
        let mismatched = ActionRecord {
            id: 1,
            chain_key: "other-chain".into(),
            service_handler_address: "handler".into(),
            event_id: "01".repeat(20),
            payload: payload.clone(),
            status: "received".into(),
        };
        for reply in [None, Some(mismatched)] {
            let backend = MockBackend {
                reply,
                requests: RefCell::default(),
                ..Default::default()
            };
            assert!(block_on(register(
                &backend,
                "dev:local",
                "handler",
                &[1; 20],
                &payload,
                MessageId {
                    uid_validity: 100,
                    uid: 10
                }
            ))
            .is_err());
        }
    }

    #[test]
    fn caches_rejected_email_only_after_backend_confirmation() {
        struct TestCache(std::cell::Cell<bool>);
        impl CacheStore for &TestCache {
            fn keys(&self) -> Result<Vec<String>> {
                Ok(Vec::new())
            }
            fn remember(&self, _: &str) -> Result<()> {
                self.0.set(true);
                Ok(())
            }
            fn remove(&self, _: &str) -> Result<()> {
                panic!("Unexpected cache removal")
            }
        }
        let writes = TestCache(std::cell::Cell::new(false));
        let cache = ProcessedEmails::load(&writes).unwrap();
        let message = MessageId {
            uid_validity: 100,
            uid: 10,
        };
        let mut backend = MockBackend::default();

        // Failed request: do not cache the email.
        assert!(block_on(reject(
            &backend,
            "dev:local",
            "handler",
            message,
            InvalidEmail::ParseFailed,
            &cache
        ))
        .is_err());
        assert!(!writes.0.get());

        backend.rejection_reply = Some(RejectionRecord {
            id: 1,
            action_id: None,
            rejection_chain_key: "dev:local".into(),
            rejection_service_handler_address: "handler".into(),
            rejection_code: "email_parse_failed".into(),
            folder: "INBOX".into(),
            uid_validity: 100,
            uid: 10,
        });

        // Confirmed rejection: cache the email.
        block_on(reject(
            &backend,
            "dev:local",
            "handler",
            message,
            InvalidEmail::ParseFailed,
            &cache,
        ))
        .unwrap();
        assert!(writes.0.get());
    }
}
