//! Reconciliation never broadcasts: fake chain reads exercise real Postgres transactions.
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use app_backend::{
    action_chain::{ActionChain, TransactionOutcome},
    action_store::{Action, ActionOutcome, ActionResult, ActionStore, CreateAction, UpdateAction},
    mailbox_store::MailboxMessageIdentity,
    reconciliation::{ReconciliationResult, ReconciliationWorker},
};
use app_contract_api::{
    service_handler::msg::{CustomExecuteMsg, UserIdEmail},
    user_registry::msg::UserId,
};
use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;

#[derive(Default)]
struct FakeChain {
    transaction: Mutex<Option<TransactionOutcome>>,
    processed: Mutex<bool>,
    unavailable: Mutex<bool>,
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl ActionChain for FakeChain {
    async fn transaction(&self, hash: &str) -> Result<TransactionOutcome> {
        self.calls.lock().unwrap().push(format!("tx:{hash}"));
        if *self.unavailable.lock().unwrap() {
            bail!("RPC unavailable")
        }
        Ok(self
            .transaction
            .lock()
            .unwrap()
            .unwrap_or(TransactionOutcome::NotFound))
    }
    async fn event_processed(&self, id: &str) -> Result<bool> {
        self.calls.lock().unwrap().push(format!("event:{id}"));
        if *self.unavailable.lock().unwrap() {
            bail!("RPC unavailable")
        }
        Ok(*self.processed.lock().unwrap())
    }
}

fn store(pool: &PgPool, chain: &str, handler: &str) -> ActionStore {
    ActionStore {
        pool: pool.clone(),
        chain_key: chain.into(),
        service_handler_address: handler.into(),
    }
}
fn worker(store: &ActionStore, chain: &Arc<FakeChain>) -> ReconciliationWorker {
    ReconciliationWorker {
        store: store.clone(),
        chain: chain.clone(),
        interval_seconds: 30,
        grace_seconds: 900,
    }
}
fn payload() -> CustomExecuteMsg {
    CustomExecuteMsg::Email(UserIdEmail {
        from: UserId::new_raw("ab".repeat(32)),
        subject: "deposit".into(),
    })
}
async fn create(store: &ActionStore, event: u8) -> i64 {
    match store
        .create(
            &format!("{event:02x}").repeat(20),
            payload(),
            Some(identity(100, u32::from(event))),
        )
        .await
        .unwrap()
    {
        CreateAction::Created(action) => action.id,
        _ => panic!("Expected new action"),
    }
}
fn identity(validity: u32, uid: u32) -> MailboxMessageIdentity {
    MailboxMessageIdentity {
        folder: "INBOX".into(),
        uid_validity: validity,
        uid,
    }
}
async fn action(store: &ActionStore, id: i64) -> Action {
    store.details(id).await.unwrap().unwrap().action
}
async fn messages(pool: &PgPool, id: i64) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT acknowledgment_status FROM action_mailbox_messages WHERE action_id=$1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap()
}
async fn age(pool: &PgPool, id: i64) {
    sqlx::query(
        "UPDATE actions SET created_at=now()-interval '1 hour',
        submitted_at=CASE WHEN submitted_at IS NOT NULL THEN now()-interval '1 hour' ELSE NULL END,
        next_check_at=now()-interval '1 second' WHERE id=$1",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}
async fn report(
    store: &ActionStore,
    id: i64,
    status: ActionOutcome,
    hash: Option<String>,
) -> UpdateAction {
    let failed = !matches!(status, ActionOutcome::Completed);
    store
        .report_result(
            id,
            ActionResult {
                status,
                tx_hash: hash.map(|value| value.to_ascii_uppercase()),
                error_code: failed.then(|| "callback_uncertain".into()),
                error_detail: None,
            },
        )
        .await
        .unwrap()
}
async fn details(store: &ActionStore, id: i64) -> Value {
    serde_json::to_value(store.details(id).await.unwrap().unwrap()).unwrap()
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn stale_received_fails_without_chain_queries_or_submission(pool: PgPool) {
    let store = store(&pool, "chain", "handler");
    let chain = Arc::new(FakeChain::default());
    let worker = worker(&store, &chain);
    let id = create(&store, 1).await;
    worker.tick().await.unwrap();
    assert_eq!(action(&store, id).await.status, "received");
    age(&pool, id).await;
    worker.tick().await.unwrap();
    let failed = action(&store, id).await;
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.error_code.as_deref(), Some("submission_not_started"));
    assert!(failed.closed_at.is_some());
    assert!(failed.submitted_at.is_none());
    assert!(failed.tx_hash.is_none());
    assert_eq!(messages(&pool, id).await, ["pending"]);
    assert!(chain.calls.lock().unwrap().is_empty());
    assert!(matches!(
        store.submission(id).await.unwrap(),
        UpdateAction::Conflict
    ));
    let before = details(&store, id).await;
    worker.tick().await.unwrap();
    assert_eq!(details(&store, id).await, before);
    assert_eq!(before["history"].as_array().unwrap().len(), 2);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn transaction_and_handled_event_queries_resolve_actions(pool: PgPool) {
    let store = store(&pool, "chain", "handler");
    for (event, outcome, processed, with_hash, expected) in [
        (1, TransactionOutcome::Succeeded, false, true, "completed"),
        (2, TransactionOutcome::Failed, false, true, "failed"),
        (3, TransactionOutcome::NotFound, true, true, "completed"),
        (4, TransactionOutcome::NotFound, true, false, "completed"),
    ] {
        let chain = Arc::new(FakeChain::default());
        *chain.transaction.lock().unwrap() = Some(outcome);
        *chain.processed.lock().unwrap() = processed;
        let id = create(&store, event).await;
        store.submission(id).await.unwrap();
        if with_hash {
            report(
                &store,
                id,
                ActionOutcome::OutcomeUnknown,
                Some(format!("{event:02x}").repeat(32)),
            )
            .await;
        }
        worker(&store, &chain).tick().await.unwrap();
        let resolved = action(&store, id).await;
        assert_eq!(resolved.status, expected);
        assert!(resolved.closed_at.is_some());
        assert!(resolved.last_checked_at.is_some());
        assert_eq!(messages(&pool, id).await, ["pending"]);
        assert_eq!(resolved.tx_hash.is_some(), with_hash);
        assert_eq!(
            chain.calls.lock().unwrap().len(),
            if with_hash && outcome == TransactionOutcome::NotFound {
                2
            } else {
                1
            }
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn grace_period_uses_submission_time_and_scheduling_survives_restart(pool: PgPool) {
    let store = store(&pool, "chain", "handler");
    let chain = Arc::new(FakeChain::default());
    let id = create(&store, 1).await;
    // An old email that was only just submitted must get the full submission grace period.
    age(&pool, id).await;
    store.submission(id).await.unwrap();
    worker(&store, &chain).tick().await.unwrap();
    let waiting = action(&store, id).await;
    assert_eq!(waiting.status, "submitting");
    assert!(waiting.closed_at.is_none());
    assert!(waiting.last_checked_at.is_some());
    assert!(waiting.next_check_at > waiting.last_checked_at.unwrap());
    assert_eq!(messages(&pool, id).await, ["waiting"]);
    let calls = chain.calls.lock().unwrap().len();
    // Reconstruct the worker and store while keeping the persisted scheduling.
    let restarted = worker(&self::store(&pool, "chain", "handler"), &chain);
    restarted.tick().await.unwrap();
    assert_eq!(chain.calls.lock().unwrap().len(), calls);
    age(&pool, id).await;
    restarted.tick().await.unwrap();
    let closed = action(&store, id).await;
    assert_eq!(closed.status, "outcome_unknown");
    assert!(closed.closed_at.is_some());
    assert_eq!(
        closed.error_code.as_deref(),
        Some("submission_outcome_unknown")
    );
    assert_eq!(messages(&pool, id).await, ["pending"]);
    let before = details(&store, id).await;
    restarted.tick().await.unwrap();
    assert_eq!(details(&store, id).await, before);
    assert!(store
        .pending_reconciliation(50, 900)
        .await
        .unwrap()
        .is_empty());
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn rpc_outages_never_close_unknown_actions_even_after_grace(pool: PgPool) {
    let store = store(&pool, "chain", "handler");
    let chain = Arc::new(FakeChain::default());
    *chain.unavailable.lock().unwrap() = true;
    for (event, with_hash) in [(1, false), (2, true)] {
        let id = create(&store, event).await;
        store.submission(id).await.unwrap();
        let hash = with_hash.then(|| format!("{event:02x}").repeat(32));
        report(&store, id, ActionOutcome::OutcomeUnknown, hash).await;
        age(&pool, id).await;
        let before = details(&store, id).await;
        worker(&store, &chain).tick().await.unwrap();
        let after = details(&store, id).await;
        assert_eq!(after["history"], before["history"]);
        assert_eq!(after["action"]["status"], "outcome_unknown");
        assert!(after["action"]["closed_at"].is_null());
        assert!(after["action"]["last_checked_at"].is_string());
        assert_eq!(messages(&pool, id).await, ["waiting"]);
    }
    *chain.unavailable.lock().unwrap() = false;
    sqlx::query("UPDATE actions SET next_check_at=now()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    worker(&store, &chain).tick().await.unwrap();
    for id in [1, 2] {
        assert!(action(&store, id).await.closed_at.is_some());
        assert_eq!(messages(&pool, id).await, ["pending"]);
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn concurrent_callbacks_and_workers_invalidate_stale_checks(pool: PgPool) {
    let store = store(&pool, "chain", "handler");
    let id = create(&store, 1).await;
    age(&pool, id).await;
    let received = store
        .pending_reconciliation(50, 900)
        .await
        .unwrap()
        .remove(0);
    store.submission(id).await.unwrap();
    assert!(!store
        .reconcile(&received, ReconciliationResult::SubmissionNotStarted, 30)
        .await
        .unwrap());
    let submitting = store
        .pending_reconciliation(50, 900)
        .await
        .unwrap()
        .remove(0);
    report(&store, id, ActionOutcome::Completed, Some("ab".repeat(32))).await;
    let completed = details(&store, id).await;
    assert!(!store
        .reconcile(&submitting, ReconciliationResult::ClosedUnknown, 30)
        .await
        .unwrap());
    assert_eq!(details(&store, id).await, completed);

    let other = create(&store, 2).await;
    store.submission(other).await.unwrap();
    let original = store
        .pending_reconciliation(50, 900)
        .await
        .unwrap()
        .remove(0);
    // A callback supplies a hash while a worker is checking the no-hash snapshot.
    report(
        &store,
        other,
        ActionOutcome::OutcomeUnknown,
        Some("cd".repeat(32)),
    )
    .await;
    assert!(!store
        .reconcile(&original, ReconciliationResult::Completed, 30)
        .await
        .unwrap());
    let current = store
        .pending_reconciliation(50, 900)
        .await
        .unwrap()
        .remove(0);
    assert!(store
        .reconcile(&current, ReconciliationResult::Deferred, 30)
        .await
        .unwrap());
    // Another worker cannot act on that snapshot after the check has been deferred.
    assert!(!store
        .reconcile(&current, ReconciliationResult::ClosedUnknown, 30)
        .await
        .unwrap());
    assert_eq!(messages(&pool, other).await, ["waiting"]);
    assert!(action(&store, other).await.closed_at.is_none());
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn reconciliation_closure_history_and_acknowledgment_are_atomic(pool: PgPool) {
    let store = store(&pool, "chain", "handler");
    let chain = Arc::new(FakeChain::default());
    *chain.processed.lock().unwrap() = true;
    let id = create(&store, 1).await;
    store.submission(id).await.unwrap();
    let before = details(&store, id).await;
    sqlx::query("ALTER TABLE action_transitions ADD CONSTRAINT reject_reconciliation CHECK (status <> 'completed') NOT VALID")
        .execute(&pool).await.unwrap();
    assert!(worker(&store, &chain).tick().await.is_err());
    assert_eq!(details(&store, id).await, before);
    assert_eq!(messages(&pool, id).await, ["waiting"]);
    sqlx::query("ALTER TABLE action_transitions DROP CONSTRAINT reject_reconciliation")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE action_mailbox_messages ADD CONSTRAINT reject_reconciliation CHECK (acknowledgment_status <> 'pending') NOT VALID")
        .execute(&pool).await.unwrap();
    assert!(worker(&store, &chain).tick().await.is_err());
    assert_eq!(details(&store, id).await, before);
    assert_eq!(messages(&pool, id).await, ["waiting"]);
    sqlx::query("ALTER TABLE action_mailbox_messages DROP CONSTRAINT reject_reconciliation")
        .execute(&pool)
        .await
        .unwrap();
    worker(&store, &chain).tick().await.unwrap();
    assert_eq!(action(&store, id).await.status, "completed");
    assert_eq!(messages(&pool, id).await, ["pending"]);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn closed_unknown_accepts_late_copies_and_confirmed_success(pool: PgPool) {
    let store = store(&pool, "chain", "handler");
    let chain = Arc::new(FakeChain::default());
    let id = create(&store, 1).await;
    store.submission(id).await.unwrap();
    report(&store, id, ActionOutcome::OutcomeUnknown, None).await;
    age(&pool, id).await;
    worker(&store, &chain).tick().await.unwrap();
    let closed = action(&store, id).await;
    assert!(closed.closed_at.is_some());
    assert!(matches!(
        store
            .create(&"01".repeat(20), payload(), Some(identity(100, 10)))
            .await
            .unwrap(),
        CreateAction::Existing(_)
    ));
    assert_eq!(messages(&pool, id).await, ["pending", "pending"]);
    assert!(matches!(
        report(&store, id, ActionOutcome::Completed, Some("ab".repeat(32))).await,
        UpdateAction::Updated(_)
    ));
    let completed = action(&store, id).await;
    assert_eq!(completed.status, "completed");
    assert_eq!(completed.closed_at, closed.closed_at);
    assert!(completed.error_code.is_none());
    assert!(matches!(
        report(&store, id, ActionOutcome::OutcomeUnknown, None).await,
        UpdateAction::Conflict
    ));
    assert!(matches!(
        report(&store, id, ActionOutcome::Failed, None).await,
        UpdateAction::Conflict
    ));
    assert!(matches!(
        report(&store, id, ActionOutcome::Completed, Some("cd".repeat(32))).await,
        UpdateAction::Conflict
    ));
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn completion_from_event_query_can_later_gain_a_transaction_hash(pool: PgPool) {
    let store = store(&pool, "chain", "handler");
    let chain = Arc::new(FakeChain::default());
    *chain.processed.lock().unwrap() = true;
    let id = create(&store, 1).await;
    store.submission(id).await.unwrap();
    worker(&store, &chain).tick().await.unwrap();
    assert_eq!(action(&store, id).await.status, "completed");
    assert!(action(&store, id).await.tx_hash.is_none());
    assert!(matches!(
        report(&store, id, ActionOutcome::Completed, Some("ab".repeat(32))).await,
        UpdateAction::Updated(_)
    ));
    assert_eq!(
        action(&store, id).await.tx_hash.as_deref(),
        Some("AB".repeat(32).as_str())
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn reconciliation_is_deployment_scoped_and_skips_terminal_and_future_records(pool: PgPool) {
    let local = store(&pool, "chain", "handler");
    let chain = Arc::new(FakeChain::default());
    for (event, chain_key, handler) in [
        (1, "chain", "handler"),
        (2, "other", "handler"),
        (3, "chain", "other"),
    ] {
        let scoped = store(&pool, chain_key, handler);
        let id = create(&scoped, event).await;
        age(&pool, id).await;
    }
    let completed = create(&local, 4).await;
    local.submission(completed).await.unwrap();
    report(
        &local,
        completed,
        ActionOutcome::Completed,
        Some("ab".repeat(32)),
    )
    .await;
    let future = create(&local, 5).await;
    local.submission(future).await.unwrap();
    sqlx::query("UPDATE actions SET next_check_at=now()+interval '1 hour' WHERE id=$1")
        .bind(future)
        .execute(&pool)
        .await
        .unwrap();
    let due = local.pending_reconciliation(50, 900).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, 1);
    assert!(!store(&pool, "other", "handler")
        .reconcile(&due[0], ReconciliationResult::SubmissionNotStarted, 30)
        .await
        .unwrap());
    worker(&local, &chain).tick().await.unwrap();
    assert_eq!(action(&local, 1).await.status, "failed");
    assert_eq!(
        action(&store(&pool, "other", "handler"), 2).await.status,
        "received"
    );
    assert_eq!(
        action(&store(&pool, "chain", "other"), 3).await.status,
        "received"
    );
    assert_eq!(action(&local, completed).await.status, "completed");
    assert_eq!(action(&local, future).await.status, "submitting");
    assert!(chain.calls.lock().unwrap().is_empty());
    assert!(local.pending_reconciliation(0, 900).await.is_err());
    assert!(local.pending_reconciliation(101, 900).await.is_err());
    assert!(local.pending_reconciliation(50, 0).await.is_err());
}
