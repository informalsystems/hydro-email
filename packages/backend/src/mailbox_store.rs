use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;

use crate::action_store::ActionStore;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailboxMessageIdentity {
    pub folder: String,
    pub uid_validity: u32,
    pub uid: u32,
}

impl MailboxMessageIdentity {
    pub fn is_valid(&self) -> bool {
        !self.folder.trim().is_empty()
            && self.folder.chars().count() <= 255
            && !self.folder.chars().any(char::is_control)
            && self.uid_validity != 0
            && self.uid != 0
    }
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct MailboxMessage {
    pub id: i64,
    pub action_id: i64,
    pub folder: String,
    pub uid_validity: i64,
    pub uid: i64,
    pub acknowledgment_status: String,
    pub attempt_count: i64,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub last_error_code: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug)]
pub enum AcknowledgmentResult {
    Acknowledged,
    Obsolete,
    Retry {
        error_code: String,
        retry_after_seconds: u32,
    },
}

pub enum UpdateAcknowledgment {
    Updated(Box<MailboxMessage>),
    NotFound,
    Conflict,
}

impl ActionStore {
    pub(crate) async fn attach_mailbox_message(
        &self,
        tx: &mut PgConnection,
        action_id: i64,
        action_status: &str,
        message: &MailboxMessageIdentity,
    ) -> Result<bool> {
        let ready = matches!(action_status, "completed" | "failed");
        let inserted: Option<i64> = sqlx::query_scalar(
            "INSERT INTO action_mailbox_messages
             (action_id, folder, uid_validity, uid,
              acknowledgment_status, next_attempt_at)
             VALUES ($1,$2,$3,$4,
                     CASE WHEN $5 THEN 'pending' ELSE 'waiting' END,
                     CASE WHEN $5 THEN now() ELSE NULL END)
             ON CONFLICT (folder, uid_validity, uid)
             DO NOTHING RETURNING action_id",
        )
        .bind(action_id)
        .bind(&message.folder)
        .bind(i64::from(message.uid_validity))
        .bind(i64::from(message.uid))
        .bind(ready)
        .fetch_optional(&mut *tx)
        .await?;
        let owner = match inserted {
            Some(id) => id,
            None => {
                sqlx::query_scalar(
                    "SELECT action_id FROM action_mailbox_messages
                     WHERE folder=$1 AND uid_validity=$2 AND uid=$3",
                )
                .bind(&message.folder)
                .bind(i64::from(message.uid_validity))
                .bind(i64::from(message.uid))
                .fetch_one(&mut *tx)
                .await?
            }
        };
        Ok(owner == action_id)
    }

    pub async fn pending_acknowledgments(&self, limit: u32) -> Result<Vec<MailboxMessage>> {
        ensure!(
            (1..=100).contains(&limit),
            "Invalid acknowledgment batch size"
        );
        Ok(sqlx::query_as(
            "SELECT m.* FROM action_mailbox_messages m
             JOIN actions a ON a.id=m.action_id
             WHERE a.chain_key=$1 AND a.service_handler_address=$2
             AND m.acknowledgment_status='pending' AND m.next_attempt_at <= now()
             ORDER BY m.next_attempt_at, m.id LIMIT $3",
        )
        .bind(&self.chain_key)
        .bind(&self.service_handler_address)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn report_acknowledgment(
        &self,
        action_id: i64,
        message_id: i64,
        result: AcknowledgmentResult,
    ) -> Result<UpdateAcknowledgment> {
        if let AcknowledgmentResult::Retry {
            error_code,
            retry_after_seconds,
        } = &result
        {
            ensure!(
                !error_code.is_empty()
                    && error_code.len() <= 64
                    && error_code
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                    && (1..=86400).contains(retry_after_seconds),
                "Invalid acknowledgment retry"
            );
        }
        let mut tx = self.pool.begin().await?;
        // Use the same action-then-message lock order as creation and result reporting.
        let action: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM actions WHERE id=$1 AND chain_key=$2
             AND service_handler_address=$3 FOR SHARE",
        )
        .bind(action_id)
        .bind(&self.chain_key)
        .bind(&self.service_handler_address)
        .fetch_optional(&mut *tx)
        .await?;
        if action.is_none() {
            return Ok(UpdateAcknowledgment::NotFound);
        }
        let current: Option<MailboxMessage> = sqlx::query_as(
            "SELECT * FROM action_mailbox_messages WHERE id=$1 AND action_id=$2 FOR UPDATE",
        )
        .bind(message_id)
        .bind(action_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(current) = current else {
            return Ok(UpdateAcknowledgment::NotFound);
        };
        if matches!(
            (current.acknowledgment_status.as_str(), &result),
            ("acknowledged", AcknowledgmentResult::Acknowledged)
                | ("obsolete", AcknowledgmentResult::Obsolete)
        ) {
            tx.commit().await?;
            return Ok(UpdateAcknowledgment::Updated(Box::new(current)));
        }
        if current.acknowledgment_status != "pending" {
            return Ok(UpdateAcknowledgment::Conflict);
        }
        let (status, error_code, delay) = match result {
            AcknowledgmentResult::Acknowledged => ("acknowledged", None, 0),
            AcknowledgmentResult::Obsolete => (
                "obsolete",
                Some("mailbox_uid_validity_changed".to_owned()),
                0,
            ),
            AcknowledgmentResult::Retry {
                error_code,
                retry_after_seconds,
            } => ("pending", Some(error_code), retry_after_seconds),
        };
        let message = sqlx::query_as(
            "UPDATE action_mailbox_messages SET
             acknowledgment_status=$2,
             acknowledged_at=CASE WHEN $2='acknowledged' THEN now() ELSE NULL END,
             attempt_count=attempt_count+1, last_attempt_at=now(), last_error_code=$3,
             next_attempt_at=CASE WHEN $2='pending' THEN now()+$4::bigint * interval '1 second' ELSE NULL END,
             updated_at=now() WHERE id=$1 RETURNING *",
        )
        .bind(message_id)
        .bind(status)
        .bind(error_code)
        .bind(i64::from(delay))
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(UpdateAcknowledgment::Updated(Box::new(message)))
    }
}
