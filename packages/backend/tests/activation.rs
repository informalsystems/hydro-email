//! Activation is an off-chain onboarding signal, committed before IMAP acknowledgment.
use app_backend::{
    action_store::ActionStore,
    activation_store::{ActivationReport, ReportActivation},
    api::{router, ApiState},
    mailbox_store::{AcknowledgmentResult, MailboxMessageIdentity, UpdateAcknowledgment},
    registration_store::RegistrationStore,
};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower::ServiceExt;

const KEY: &str = "test-credential-at-least-32-characters";

fn store(pool: &PgPool) -> RegistrationStore {
    RegistrationStore {
        pool: pool.clone(),
        chain_key: "dev:local".into(),
        registry_address: "registry".into(),
    }
}

fn actions(pool: &PgPool) -> ActionStore {
    ActionStore {
        pool: pool.clone(),
        chain_key: "dev:local".into(),
        service_handler_address: "handler".into(),
    }
}

fn app(pool: &PgPool) -> Router {
    router(ApiState {
        store: store(pool),
        actions: actions(pool),
        api_key_hash: Sha256::digest(KEY).into(),
        hourly_quota: 10,
        daily_quota: 100,
    })
}

fn report(uid: u32, verified: bool) -> ActivationReport {
    ActivationReport {
        verified,
        error_code: (!verified).then(|| "email_authentication_failed".into()),
        mailbox_message: MailboxMessageIdentity {
            folder: "INBOX".into(),
            uid_validity: 100,
            uid,
        },
    }
}

async fn admit(pool: &PgPool) -> String {
    let id = "01".repeat(32);
    store(pool)
        .admit("alice@example.com", &id, 10, 100)
        .await
        .unwrap();
    id
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    value: Value,
    authenticated: bool,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if authenticated {
        req = req.header("authorization", format!("Bearer {KEY}"));
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(value.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let value =
        serde_json::from_slice(&to_bytes(response.into_body(), 16384).await.unwrap()).unwrap();
    (status, value)
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn successful_activation_is_idempotent_and_acknowledged_after_commit(pool: PgPool) {
    let id = admit(&pool).await;
    let state = store(&pool).get(&id).await.unwrap().unwrap();
    assert!(state.activated_at.is_none());
    let input = report(1, true);
    assert!(matches!(
        store(&pool)
            .report_activation("handler", &id, &input)
            .await
            .unwrap(),
        ReportActivation::Created(_)
    ));
    let activated = store(&pool).get(&id).await.unwrap().unwrap();
    assert!(activated.activated_at.is_some());
    assert_eq!(activated.status, "queued"); // Provisioning and activation are independent.
    assert!(matches!(
        store(&pool)
            .report_activation("handler", &id, &input)
            .await
            .unwrap(),
        ReportActivation::Existing(_)
    ));
    assert_eq!(
        activated.activated_at,
        store(&pool).get(&id).await.unwrap().unwrap().activated_at
    );
    let pending = actions(&pool).pending_acknowledgments(25).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].activation_id.is_some());
    assert!(pending[0].action_id.is_none());
    assert!(matches!(
        actions(&pool)
            .report_acknowledgment(None, pending[0].id, AcknowledgmentResult::Acknowledged)
            .await
            .unwrap(),
        UpdateAcknowledgment::Updated(_)
    ));
    assert!(actions(&pool)
        .pending_acknowledgments(25)
        .await
        .unwrap()
        .is_empty());
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM registration_activations), (SELECT count(*) FROM actions)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (1, 0));
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn failed_reports_never_revoke_activation_or_override_later_success(pool: PgPool) {
    let id = admit(&pool).await;
    let failure = report(1, false);
    store(&pool)
        .report_activation("handler", &id, &failure)
        .await
        .unwrap();
    let state = store(&pool).get(&id).await.unwrap().unwrap();
    assert!(state.activated_at.is_none());
    assert_eq!(
        state.activation_error_code.as_deref(),
        Some("email_authentication_failed")
    );
    store(&pool)
        .report_activation("handler", &id, &report(2, true))
        .await
        .unwrap();
    let activated_at = store(&pool).get(&id).await.unwrap().unwrap().activated_at;
    assert!(activated_at.is_some());
    // A late replay and a new failed message both preserve the successful state.
    store(&pool)
        .report_activation("handler", &id, &failure)
        .await
        .unwrap();
    store(&pool)
        .report_activation("handler", &id, &report(3, false))
        .await
        .unwrap();
    let state = store(&pool).get(&id).await.unwrap().unwrap();
    assert_eq!(state.activated_at, activated_at);
    assert!(state.activation_error_code.is_none());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM registration_activations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 3);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn conflicting_uid_rolls_back_activation_and_keeps_original_owner(pool: PgPool) {
    let id = admit(&pool).await;
    actions(&pool)
        .reject_mailbox_message(&report(1, true).mailbox_message, "email_parse_failed")
        .await
        .unwrap();
    assert!(matches!(
        store(&pool)
            .report_activation("handler", &id, &report(1, true))
            .await
            .unwrap(),
        ReportActivation::Conflict
    ));
    assert!(store(&pool)
        .get(&id)
        .await
        .unwrap()
        .unwrap()
        .activated_at
        .is_none());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM registration_activations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    store(&pool)
        .report_activation("handler", &id, &report(2, false))
        .await
        .unwrap();
    assert!(matches!(
        store(&pool)
            .report_activation("handler", &id, &report(2, true))
            .await
            .unwrap(),
        ReportActivation::Conflict
    ));
    assert!(matches!(
        store(&pool)
            .report_activation("other-handler", &id, &report(2, false))
            .await
            .unwrap(),
        ReportActivation::Conflict
    ));
    assert!(store(&pool)
        .get(&id)
        .await
        .unwrap()
        .unwrap()
        .activated_at
        .is_none());
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn activation_acknowledgments_are_scoped_to_the_handler(pool: PgPool) {
    let id = admit(&pool).await;
    store(&pool)
        .report_activation("handler", &id, &report(1, true))
        .await
        .unwrap();
    let pending = actions(&pool).pending_acknowledgments(25).await.unwrap();
    let mut other = actions(&pool);
    other.service_handler_address = "other".into();
    assert!(other.pending_acknowledgments(25).await.unwrap().is_empty());
    assert!(matches!(
        other
            .report_acknowledgment(None, pending[0].id, AcknowledgmentResult::Acknowledged)
            .await
            .unwrap(),
        UpdateAcknowledgment::NotFound
    ));
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn concurrent_reports_are_idempotent(pool: PgPool) {
    let id = admit(&pool).await;
    let store = store(&pool);
    let input = report(1, true);
    let (first, second) = tokio::join!(
        store.report_activation("handler", &id, &input),
        store.report_activation("handler", &id, &input)
    );
    assert!(matches!(
        (first.unwrap(), second.unwrap()),
        (ReportActivation::Created(_), ReportActivation::Existing(_))
            | (ReportActivation::Existing(_), ReportActivation::Created(_))
    ));
    assert_eq!(
        actions(&pool)
            .pending_acknowledgments(25)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn api_requires_auth_registered_identity_and_valid_result(pool: PgPool) {
    let id = admit(&pool).await;
    let app = app(&pool);
    let path = format!("/v1/registrations/{id}/activation");
    let input =
        json!({"verified": true, "mailbox_message": {"folder":"INBOX","uid_validity":100,"uid":1}});
    assert_eq!(
        request(&app, "POST", &path, input.clone(), false).await.0,
        StatusCode::UNAUTHORIZED
    );
    for invalid in [
        json!({"verified":false,"mailbox_message":{"folder":"INBOX","uid_validity":100,"uid":1}}),
        json!({"verified":true,"error_code":"email_authentication_failed","mailbox_message":{"folder":"INBOX","uid_validity":100,"uid":1}}),
        json!({"verified":true,"mailbox_message":{"folder":"INBOX","uid_validity":100,"uid":0}}),
    ] {
        assert_eq!(
            request(&app, "POST", &path, invalid, true).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let unknown = format!("/v1/registrations/{}/activation", "02".repeat(32));
    assert_eq!(
        request(&app, "POST", &unknown, input.clone(), true).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, created) = request(&app, "POST", &path, input.clone(), true).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, existing) = request(&app, "POST", &path, input, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created, existing);
    let (_, registration) = request(
        &app,
        "GET",
        &format!("/v1/registrations/{id}"),
        Value::Null,
        true,
    )
    .await;
    assert!(registration["activated_at"].is_string());
    assert!(registration["activation_error_code"].is_null());
    assert_eq!(registration["status"], "queued");
}
