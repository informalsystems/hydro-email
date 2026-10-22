use std::{sync::Arc, time::Duration};

use anyhow::{ensure, Result};
use chrono::Utc;

use crate::{
    action_chain::{ActionChain, TransactionOutcome},
    action_store::{Action, ActionStore},
};

#[derive(Debug, Clone, Copy)]
pub enum ReconciliationResult {
    Deferred,
    Completed,
    Failed,
    SubmissionNotStarted,
    ClosedUnknown,
}

impl ActionStore {
    pub async fn pending_reconciliation(
        &self,
        limit: u32,
        grace_seconds: u32,
    ) -> Result<Vec<Action>> {
        ensure!(
            (1..=100).contains(&limit) && grace_seconds > 0,
            "Invalid reconciliation settings"
        );
        Ok(sqlx::query_as(
            "SELECT * FROM actions WHERE chain_key=$1 AND service_handler_address=$2
             AND closed_at IS NULL AND status IN ('received','submitting','outcome_unknown')
             AND next_check_at <= now()
             AND (status <> 'received' OR created_at <= now()-$3::bigint * interval '1 second')
             ORDER BY next_check_at, id LIMIT $4",
        )
        .bind(&self.chain_key)
        .bind(&self.service_handler_address)
        .bind(i64::from(grace_seconds))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?)
    }

    // Chain checks happen outside a DB transaction. Reject stale observations
    // after a callback or another worker changes the record in the meantime.
    pub async fn reconcile(
        &self,
        expected: &Action,
        result: ReconciliationResult,
        interval_seconds: u32,
    ) -> Result<bool> {
        ensure!(interval_seconds > 0, "Invalid reconciliation interval");
        let mut tx = self.pool.begin().await?;
        let current: Option<Action> = sqlx::query_as(
            "SELECT * FROM actions WHERE id=$1 AND chain_key=$2 AND service_handler_address=$3 FOR UPDATE",
        ).bind(expected.id).bind(&self.chain_key).bind(&self.service_handler_address)
            .fetch_optional(&mut *tx).await?;
        let Some(current) = current else {
            return Ok(false);
        };
        if current.closed_at.is_some()
            || !matches!(
                current.status.as_str(),
                "received" | "submitting" | "outcome_unknown"
            )
            || current.status != expected.status
            || current.tx_hash != expected.tx_hash
            || current.updated_at != expected.updated_at
            || current.created_at != expected.created_at
            || current.submitted_at != expected.submitted_at
            || current.next_check_at != expected.next_check_at
            || current.last_checked_at != expected.last_checked_at
        {
            return Ok(false);
        }
        if matches!(result, ReconciliationResult::Deferred) {
            sqlx::query(
                "UPDATE actions SET last_checked_at=now(),
                 next_check_at=now()+$2::bigint * interval '1 second' WHERE id=$1",
            )
            .bind(current.id)
            .bind(i64::from(interval_seconds))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(true);
        }
        let (status, error_code, error_detail) = match result {
            ReconciliationResult::Completed => ("completed", None, None),
            ReconciliationResult::Failed => ("failed", Some("transaction_failed"), Some("Transaction execution failed.")),
            ReconciliationResult::SubmissionNotStarted => {
                ensure!(current.status == "received", "Action submission already started");
                ("failed", Some("submission_not_started"), Some("Submission intent was not recorded before the grace period expired."))
            }
            ReconciliationResult::ClosedUnknown => ("outcome_unknown", Some("submission_outcome_unknown"), Some("Execution could not be confirmed before the reconciliation grace period expired.")),
            ReconciliationResult::Deferred => unreachable!(),
        };
        sqlx::query(
            "UPDATE actions SET status=$2, error_code=$3, error_detail=$4,
             closed_at=now(), last_checked_at=now(), updated_at=now() WHERE id=$1",
        )
        .bind(current.id)
        .bind(status)
        .bind(error_code)
        .bind(error_detail)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO action_transitions (action_id,status,error_code,error_detail) VALUES ($1,$2,$3,$4)",
        ).bind(current.id).bind(status).bind(error_code).bind(error_detail)
            .execute(&mut *tx).await?;
        sqlx::query(
            "UPDATE action_mailbox_messages SET acknowledgment_status='pending',
             next_attempt_at=now(), updated_at=now()
             WHERE action_id=$1 AND acknowledgment_status='waiting'",
        )
        .bind(current.id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        tracing::info!(action_id=current.id, event_id=%current.event_id, status,
            error_code, "Action reconciliation closed");
        Ok(true)
    }
}

pub struct ReconciliationWorker {
    pub store: ActionStore,
    pub chain: Arc<dyn ActionChain>,
    pub interval_seconds: u32,
    pub grace_seconds: u32,
}

impl ReconciliationWorker {
    pub async fn run(&self) -> Result<()> {
        ensure!(
            self.interval_seconds > 0 && self.grace_seconds > 0,
            "Invalid reconciliation settings"
        );
        loop {
            self.tick().await?;
            tokio::time::sleep(Duration::from_secs(u64::from(self.interval_seconds))).await;
        }
    }

    pub async fn tick(&self) -> Result<()> {
        for action in self
            .store
            .pending_reconciliation(25, self.grace_seconds)
            .await?
        {
            let result = if action.status == "received" {
                ReconciliationResult::SubmissionNotStarted
            } else {
                match tokio::time::timeout(Duration::from_secs(15), self.check(&action)).await {
                    Ok(Ok(result)) => result,
                    _ => {
                        // No raw RPC errors or URLs: they may contain credentials.
                        tracing::warn!(
                            action_id = action.id,
                            "Action chain check unavailable; retry scheduled"
                        );
                        ReconciliationResult::Deferred
                    }
                }
            };
            self.store
                .reconcile(&action, result, self.interval_seconds)
                .await?;
        }
        Ok(())
    }

    async fn check(&self, action: &Action) -> Result<ReconciliationResult> {
        if let Some(hash) = &action.tx_hash {
            match self.chain.transaction(hash).await? {
                TransactionOutcome::Succeeded => return Ok(ReconciliationResult::Completed),
                TransactionOutcome::Failed => return Ok(ReconciliationResult::Failed),
                TransactionOutcome::NotFound => {}
            }
        }
        if self.chain.event_processed(&action.event_id).await? {
            return Ok(ReconciliationResult::Completed);
        }
        let start = action.submitted_at.unwrap_or(action.created_at);
        let expired =
            Utc::now().signed_duration_since(start).num_seconds() >= i64::from(self.grace_seconds);
        Ok(if expired {
            ReconciliationResult::ClosedUnknown
        } else {
            ReconciliationResult::Deferred
        })
    }
}
