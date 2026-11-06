use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    mailbox_store::{valid_error_code, MailboxMessageIdentity},
    registration_store::RegistrationStore,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationReport {
    pub verified: bool,
    pub error_code: Option<String>,
    pub mailbox_message: MailboxMessageIdentity,
}

impl ActivationReport {
    pub fn is_valid(&self) -> bool {
        self.mailbox_message.is_valid()
            && match &self.error_code {
                None => self.verified,
                Some(code) => !self.verified && valid_error_code(code),
            }
    }
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Activation {
    pub id: i64,
    pub chain_key: String,
    pub registry_address: String,
    pub service_handler_address: String,
    pub user_id: String,
    pub verified: bool,
    pub error_code: Option<String>,
    pub folder: String,
    pub uid_validity: i64,
    pub uid: i64,
    pub created_at: DateTime<Utc>,
}

pub enum ReportActivation {
    Created(Activation),
    Existing(Activation),
    NotFound,
    Conflict,
}

impl RegistrationStore {
    pub async fn report_activation(
        &self,
        handler: &str,
        user_id: &str,
        report: &ActivationReport,
    ) -> Result<ReportActivation> {
        ensure!(report.is_valid(), "Invalid activation report");
        let mut tx = self.pool.begin().await?;
        // Serialize reports for a registration, including concurrent success/failure.
        let exists: Option<String> = sqlx::query_scalar(
            "SELECT user_id FROM registrations WHERE chain_key=$1
             AND registry_address=$2 AND user_id=$3 FOR UPDATE",
        )
        .bind(&self.chain_key)
        .bind(&self.registry_address)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        if exists.is_none() {
            return Ok(ReportActivation::NotFound);
        }
        let message = &report.mailbox_message;
        let inserted: Option<Activation> = sqlx::query_as(
            "INSERT INTO registration_activations
             (chain_key,registry_address,service_handler_address,user_id,verified,error_code,
              folder,uid_validity,uid) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
             ON CONFLICT (folder,uid_validity,uid) DO NOTHING RETURNING *",
        )
        .bind(&self.chain_key)
        .bind(&self.registry_address)
        .bind(handler)
        .bind(user_id)
        .bind(report.verified)
        .bind(&report.error_code)
        .bind(&message.folder)
        .bind(i64::from(message.uid_validity))
        .bind(i64::from(message.uid))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(activation) = inserted else {
            let current: Activation = sqlx::query_as(
                "SELECT * FROM registration_activations WHERE folder=$1 AND uid_validity=$2 AND uid=$3",
            )
            .bind(&message.folder)
            .bind(i64::from(message.uid_validity))
            .bind(i64::from(message.uid))
            .fetch_one(&mut *tx)
            .await?;
            let matches = current.chain_key == self.chain_key
                && current.registry_address == self.registry_address
                && current.service_handler_address == handler
                && current.user_id == user_id
                && current.verified == report.verified
                && current.error_code == report.error_code;
            return Ok(if matches {
                ReportActivation::Existing(current)
            } else {
                ReportActivation::Conflict
            });
        };
        // Queue acknowledgment in the same transaction: a persisted report can
        // always be marked read, even if the operator exits immediately afterward.
        let mailbox_id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO action_mailbox_messages
             (activation_id,folder,uid_validity,uid,acknowledgment_status,next_attempt_at)
             VALUES ($1,$2,$3,$4,'pending',now())
             ON CONFLICT (folder,uid_validity,uid) DO NOTHING RETURNING id",
        )
        .bind(activation.id)
        .bind(&message.folder)
        .bind(i64::from(message.uid_validity))
        .bind(i64::from(message.uid))
        .fetch_optional(&mut *tx)
        .await?;
        if mailbox_id.is_none() {
            return Ok(ReportActivation::Conflict);
        }
        sqlx::query(
            "UPDATE registrations SET
             activated_at=CASE WHEN $4 THEN COALESCE(activated_at,now()) ELSE activated_at END,
             activation_error_code=CASE WHEN $4 OR activated_at IS NOT NULL THEN NULL ELSE $5 END,
             updated_at=now() WHERE chain_key=$1 AND registry_address=$2 AND user_id=$3",
        )
        .bind(&self.chain_key)
        .bind(&self.registry_address)
        .bind(user_id)
        .bind(report.verified)
        .bind(&report.error_code)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ReportActivation::Created(activation))
    }
}
