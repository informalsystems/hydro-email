use anyhow::Result;
use app_contract_api::service_handler::msg::CustomExecuteMsg;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{types::Json, PgPool};

use crate::mailbox_store::MailboxMessageIdentity;

#[derive(Clone)]
pub struct ActionStore {
    pub pool: PgPool,
    pub chain_key: String,
    pub service_handler_address: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Action {
    pub id: i64,
    pub chain_key: String,
    pub service_handler_address: String,
    pub event_id: String,
    pub user_id: String,
    pub payload: Json<CustomExecuteMsg>,
    pub status: String,
    pub tx_hash: Option<String>,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub next_check_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub enum CreateAction {
    Created(Action),
    Existing(Action),
    Conflict,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ActionTransition {
    pub id: i64,
    pub action_id: i64,
    pub status: String,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ActionDetails {
    pub action: Action,
    pub history: Vec<ActionTransition>,
}

#[derive(Debug, Serialize)]
pub struct ActionPage {
    pub actions: Vec<Action>,
    pub next_before_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionOutcome {
    Completed,
    Failed,
    OutcomeUnknown,
}

impl ActionOutcome {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionResult {
    pub status: ActionOutcome,
    pub tx_hash: Option<String>,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
}

pub enum UpdateAction {
    Updated(Box<Action>),
    NotFound,
    Conflict,
}

impl ActionStore {
    pub async fn details(&self, id: i64) -> Result<Option<ActionDetails>> {
        let mut tx = self.pool.begin().await?;
        // Hold the action stable while reading its matching history.
        let action = sqlx::query_as(
            "SELECT * FROM actions WHERE id=$1 AND chain_key=$2 AND service_handler_address=$3 FOR SHARE",
        )
        .bind(id).bind(&self.chain_key).bind(&self.service_handler_address)
        .fetch_optional(&mut *tx).await?;
        let Some(action) = action else {
            return Ok(None);
        };
        let history =
            sqlx::query_as("SELECT * FROM action_transitions WHERE action_id=$1 ORDER BY id")
                .bind(id)
                .fetch_all(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(Some(ActionDetails { action, history }))
    }

    pub async fn list(
        &self,
        user_id: &str,
        before_id: Option<i64>,
        limit: u32,
    ) -> Result<ActionPage> {
        let mut actions: Vec<Action> = sqlx::query_as(
            "SELECT * FROM actions WHERE chain_key=$1 AND service_handler_address=$2 AND user_id=$3
             AND ($4::bigint IS NULL OR id < $4) ORDER BY id DESC LIMIT $5",
        )
        .bind(&self.chain_key)
        .bind(&self.service_handler_address)
        .bind(user_id)
        .bind(before_id)
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await?;
        let has_more = actions.len() > limit as usize;
        actions.truncate(limit as usize);
        let next_before_id = if has_more {
            actions.last().map(|action| action.id)
        } else {
            None
        };
        Ok(ActionPage {
            actions,
            next_before_id,
        })
    }

    pub async fn submission(&self, id: i64) -> Result<UpdateAction> {
        self.update(id, "submitting", None, None, None).await
    }

    pub async fn report_result(&self, id: i64, result: ActionResult) -> Result<UpdateAction> {
        self.update(
            id,
            result.status.as_str(),
            result.tx_hash,
            result.error_code,
            result.error_detail,
        )
        .await
    }

    async fn update(
        &self,
        id: i64,
        status: &str,
        tx_hash: Option<String>,
        error_code: Option<String>,
        error_detail: Option<String>,
    ) -> Result<UpdateAction> {
        let mut tx = self.pool.begin().await?;
        let current: Option<Action> = sqlx::query_as(
            "SELECT * FROM actions WHERE id=$1 AND chain_key=$2 AND service_handler_address=$3 FOR UPDATE",
        )
        .bind(id).bind(&self.chain_key).bind(&self.service_handler_address)
        .fetch_optional(&mut *tx).await?;
        let Some(current) = current else {
            return Ok(UpdateAction::NotFound);
        };
        if current
            .tx_hash
            .as_ref()
            .zip(tx_hash.as_ref())
            .is_some_and(|(saved, new)| saved != new)
        {
            return Ok(UpdateAction::Conflict);
        }
        let tx_hash = tx_hash.or_else(|| current.tx_hash.clone());
        if current.status == status
            && current.tx_hash == tx_hash
            && current.error_code == error_code
            && current.error_detail == error_detail
        {
            tx.commit().await?;
            return Ok(UpdateAction::Updated(Box::new(current)));
        }
        let allowed = match status {
            "submitting" => current.status == "received",
            // Late success can resolve failure or supply a hash for a completion
            // previously established only through the handled-event query.
            "completed" => {
                matches!(
                    current.status.as_str(),
                    "submitting" | "outcome_unknown" | "failed"
                ) || (current.status == "completed"
                    && current.tx_hash.is_none()
                    && tx_hash.is_some())
            }
            _ => matches!(current.status.as_str(), "submitting" | "outcome_unknown"),
        };
        if !allowed {
            return Ok(UpdateAction::Conflict);
        }
        let action = sqlx::query_as(
            "UPDATE actions SET status=$2, tx_hash=$3, error_code=$4, error_detail=$5,
             submitted_at=CASE WHEN $2='submitting' THEN now() ELSE submitted_at END,
             closed_at=CASE WHEN $2 IN ('completed','failed') THEN COALESCE(closed_at,now()) ELSE closed_at END,
             updated_at=now(), next_check_at=now() WHERE id=$1 RETURNING *",
        )
        .bind(id)
        .bind(status)
        .bind(tx_hash)
        .bind(&error_code)
        .bind(&error_detail)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO action_transitions (action_id, status, error_code, error_detail) VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(status)
        .bind(error_code)
        .bind(error_detail)
        .execute(&mut *tx)
        .await?;
        if matches!(status, "completed" | "failed") || current.closed_at.is_some() {
            sqlx::query(
                "UPDATE action_mailbox_messages SET acknowledgment_status='pending',
                 next_attempt_at=now(), updated_at=now()
                 WHERE action_id=$1 AND acknowledgment_status='waiting'",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(UpdateAction::Updated(Box::new(action)))
    }

    pub async fn create(
        &self,
        event_id: &str,
        payload: CustomExecuteMsg,
        mailbox_message: Option<MailboxMessageIdentity>,
    ) -> Result<CreateAction> {
        let CustomExecuteMsg::Email(email) = &payload;
        let mut tx = self.pool.begin().await?;
        let created: Option<Action> = sqlx::query_as(
            "INSERT INTO actions (chain_key, service_handler_address, event_id, user_id, payload)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (chain_key, service_handler_address, event_id) DO NOTHING
             RETURNING *",
        )
        .bind(&self.chain_key)
        .bind(&self.service_handler_address)
        .bind(event_id)
        .bind(email.from.as_str())
        .bind(Json(&payload))
        .fetch_optional(&mut *tx)
        .await?;

        let is_new = created.is_some();
        let action = if let Some(action) = created {
            sqlx::query(
                "INSERT INTO action_transitions (action_id, status) VALUES ($1, 'received')",
            )
            .bind(action.id)
            .execute(&mut *tx)
            .await?;
            action
        } else {
            // A separate statement sees a concurrent creator's committed row.
            sqlx::query_as::<_, Action>(
            "SELECT * FROM actions WHERE chain_key=$1 AND service_handler_address=$2 AND event_id=$3 FOR UPDATE",
        )
        .bind(&self.chain_key)
        .bind(&self.service_handler_address)
        .bind(event_id)
        .fetch_one(&mut *tx)
        .await?
        };
        if action.user_id != email.from.as_str() || action.payload.0 != payload {
            return Ok(CreateAction::Conflict);
        }
        if let Some(message) = mailbox_message {
            if !self
                .attach_mailbox_message(&mut tx, &action, &message)
                .await?
            {
                return Ok(CreateAction::Conflict);
            }
        }
        tx.commit().await?;
        Ok(if is_new {
            CreateAction::Created(action)
        } else {
            CreateAction::Existing(action)
        })
    }
}
