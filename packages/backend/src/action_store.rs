use anyhow::Result;
use app_contract_api::service_handler::msg::CustomExecuteMsg;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{types::Json, PgPool};

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
    pub submitted_at: Option<DateTime<Utc>>,
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

impl ActionStore {
    pub async fn create(&self, event_id: &str, payload: CustomExecuteMsg) -> Result<CreateAction> {
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

        if let Some(action) = created {
            sqlx::query(
                "INSERT INTO action_transitions (action_id, status) VALUES ($1, 'received')",
            )
            .bind(action.id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(CreateAction::Created(action));
        }

        // A separate statement sees a concurrent creator's committed row.
        let action: Action = sqlx::query_as(
            "SELECT * FROM actions WHERE chain_key=$1 AND service_handler_address=$2 AND event_id=$3",
        )
        .bind(&self.chain_key)
        .bind(&self.service_handler_address)
        .bind(event_id)
        .fetch_one(&mut *tx)
        .await?;
        if action.user_id != email.from.as_str() || action.payload.0 != payload {
            return Ok(CreateAction::Conflict);
        }
        tx.commit().await?;
        Ok(CreateAction::Existing(action))
    }
}
