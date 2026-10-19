//! Action API, persistence, and database constraint tests.
use app_backend::{
    action_store::ActionStore,
    api::{router, ApiState},
    registration_store::RegistrationStore,
};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    response::Response,
    Router,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{error::ErrorKind, PgPool};
use tower::ServiceExt;

const KEY: &str = "test-credential-at-least-32-characters";

fn app(pool: &PgPool, chain: &str, handler: &str) -> Router {
    router(ApiState {
        store: RegistrationStore {
            pool: pool.clone(),
            chain_key: chain.into(),
            registry_address: "registry".into(),
        },
        actions: ActionStore {
            pool: pool.clone(),
            chain_key: chain.into(),
            service_handler_address: handler.into(),
        },
        api_key_hash: Sha256::digest(KEY).into(),
        hourly_quota: 10,
        daily_quota: 100,
    })
}

fn action_request() -> Value {
    json!({
        "event_id": "01".repeat(20),
        "payload": {"email": {"from": "02".repeat(32), "subject": "deposit"}}
    })
}

async fn post(app: &Router, body: &Value, key: Option<&str>) -> Response {
    let mut request = Request::post("/v1/actions").header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    app.clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

async fn body(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 16_384).await.unwrap()).unwrap()
}

async fn assert_counts(pool: &PgPool, actions: i64, transitions: i64) {
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM actions),
                (SELECT count(*) FROM action_transitions)",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(counts, (actions, transitions));
}

async fn create_action(
    pool: &PgPool,
    chain: &str,
    handler: &str,
    event_id: &str,
    user_id: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO actions (chain_key, service_handler_address, event_id, user_id, payload)
         VALUES ($1, $2, $3, $4, '{\"email\":{\"subject\":\"deposit\"}}') RETURNING id",
    )
    .bind(chain)
    .bind(handler)
    .bind(event_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
}

fn assert_database_error<T: std::fmt::Debug>(result: Result<T, sqlx::Error>, expected: ErrorKind) {
    let err = result.unwrap_err();
    assert_eq!(
        err.as_database_error().map(|err| err.kind()),
        Some(expected),
        "{err:?}"
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn concurrent_action_requests_create_one_action_and_initial_transition(pool: PgPool) {
    let app = app(&pool, "dev:local", "handler");
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            let response = post(&app, &action_request(), Some(KEY)).await;
            (response.status(), body(response).await)
        }));
    }
    let mut created = 0;
    let mut ids = Vec::new();
    for task in tasks {
        let (status, action) = task.await.unwrap();
        match status {
            StatusCode::CREATED => created += 1,
            StatusCode::OK => {}
            _ => panic!("Unexpected response: {status} {action}"),
        }
        ids.push(action["id"].as_i64().unwrap());
        assert_eq!(action["chain_key"], "dev:local");
        assert_eq!(action["service_handler_address"], "handler");
        assert_eq!(action["event_id"], "01".repeat(20));
        assert_eq!(action["user_id"], "02".repeat(32));
        assert_eq!(action["payload"], action_request()["payload"]);
        assert_eq!(action["status"], "received");
        assert!(action["tx_hash"].is_null());
        assert!(action["error_code"].is_null());
        assert!(action["submitted_at"].is_null());
        assert!(action["last_checked_at"].is_null());
        assert!(action["next_check_at"].is_string());
        assert!(action["created_at"].is_string());
        assert!(action["updated_at"].is_string());
    }
    assert_eq!(created, 1);
    assert!(ids.iter().all(|id| *id == ids[0]));
    assert_counts(&pool, 1, 1).await;
    let transition: (i64, String) =
        sqlx::query_as("SELECT action_id, status FROM action_transitions")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(transition, (ids[0], "received".into()));
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn duplicate_actions_preserve_state_and_conflicting_payloads_are_rejected(pool: PgPool) {
    let app = app(&pool, "dev:local", "handler");
    let request = action_request();
    let response = post(&app, &request, Some(KEY)).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let original = body(response).await;
    let id = original["id"].as_i64().unwrap();
    // A repeated create must not reset a record whose processing has progressed.
    sqlx::query("UPDATE actions SET status='completed' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let response = post(&app, &request, Some(KEY)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let existing = body(response).await;
    assert_eq!(existing["id"], original["id"]);
    assert_eq!(existing["created_at"], original["created_at"]);
    assert_eq!(existing["updated_at"], original["updated_at"]);
    assert_eq!(existing["status"], "completed");

    for (field, value) in [("subject", "forward".into()), ("from", "03".repeat(32))] {
        let mut conflicting = request.clone();
        conflicting["payload"]["email"][field] = json!(value);
        let response = post(&app, &conflicting, Some(KEY)).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(body(response).await, json!({"error": "action_conflict"}));
    }
    let response = post(&app, &request, Some(KEY)).await;
    assert_eq!(body(response).await, existing);
    assert_counts(&pool, 1, 1).await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn action_requests_validate_authentication_identifiers_and_shape(pool: PgPool) {
    let app = app(&pool, "dev:local", "handler");
    for key in [None, Some("wrong-key")] {
        assert_eq!(
            post(&app, &action_request(), key).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    for field in ["event_id", "from"] {
        let valid_length = if field == "event_id" { 20 } else { 32 };
        for invalid in [
            "01".repeat(valid_length - 1),
            "01".repeat(valid_length + 1),
            "AB".repeat(valid_length),
            "gg".repeat(valid_length),
        ] {
            let mut request = action_request();
            if field == "event_id" {
                request[field] = json!(invalid);
            } else {
                request["payload"]["email"][field] = json!(invalid);
            }
            let response = post(&app, &request, Some(KEY)).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let expected = if field == "event_id" {
                "invalid_event_id"
            } else {
                "invalid_user_id"
            };
            assert_eq!(body(response).await, json!({"error": expected}));
        }
    }
    for request in [
        json!({"event_id": "01".repeat(20)}),
        json!({"event_id": "01".repeat(20), "payload": {"unknown": {}}}),
        json!({"event_id": "01".repeat(20), "payload": {"email": {"from": "02".repeat(32)}}}),
        {
            let mut request = action_request();
            request["user_id"] = json!("03".repeat(32));
            request
        },
        {
            let mut request = action_request();
            request["chain_key"] = json!("untrusted-chain");
            request
        },
    ] {
        assert_eq!(
            post(&app, &request, Some(KEY)).await.status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let mut oversized = action_request();
    oversized["payload"]["email"]["subject"] = json!("a".repeat(4096));
    assert_eq!(
        post(&app, &oversized, Some(KEY)).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_counts(&pool, 0, 0).await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn action_namespace_comes_from_backend_configuration(pool: PgPool) {
    for (chain, handler) in [
        ("dev:local", "handler"),
        ("dev:other", "handler"),
        ("dev:local", "other-handler"),
    ] {
        let app = app(&pool, chain, handler);
        let response = post(&app, &action_request(), Some(KEY)).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let action = body(response).await;
        assert_eq!(action["chain_key"], chain);
        assert_eq!(action["service_handler_address"], handler);
    }
    let mut next_event = action_request();
    next_event["event_id"] = json!("03".repeat(20));
    let local = app(&pool, "dev:local", "handler");
    assert_eq!(
        post(&local, &next_event, Some(KEY)).await.status(),
        StatusCode::CREATED
    );
    assert_counts(&pool, 4, 4).await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn failed_initial_history_write_rolls_back_action_creation(pool: PgPool) {
    let app = app(&pool, "dev:local", "handler");
    // Force failure after the action insert, during the initial history write.
    sqlx::query("ALTER TABLE action_transitions ADD CONSTRAINT reject_received CHECK (status <> 'received')")
        .execute(&pool).await.unwrap();
    let response = post(&app, &action_request(), Some(KEY)).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body(response).await,
        json!({"error": "backend_unavailable"})
    );
    assert_counts(&pool, 0, 0).await;
    sqlx::query("ALTER TABLE action_transitions DROP CONSTRAINT reject_received")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        post(&app, &action_request(), Some(KEY)).await.status(),
        StatusCode::CREATED
    );
    assert_counts(&pool, 1, 1).await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn action_identifiers_and_statuses_are_validated(pool: PgPool) {
    let event_id = "01".repeat(20);
    let user_id = "02".repeat(32);
    for invalid in [
        "01".repeat(19),
        "01".repeat(21),
        "gg".repeat(20),
        "AB".repeat(20),
    ] {
        assert_database_error(
            create_action(&pool, "dev:local", "handler", &invalid, &user_id).await,
            ErrorKind::CheckViolation,
        );
    }
    for invalid in [
        "02".repeat(31),
        "02".repeat(33),
        "gg".repeat(32),
        "AB".repeat(32),
    ] {
        assert_database_error(
            create_action(&pool, "dev:local", "handler", &event_id, &invalid).await,
            ErrorKind::CheckViolation,
        );
    }
    let id = create_action(&pool, "dev:local", "handler", &event_id, &user_id)
        .await
        .unwrap();
    for status in ["pending", "invalid"] {
        assert_database_error(
            sqlx::query("UPDATE actions SET status=$2 WHERE id=$1")
                .bind(id)
                .bind(status)
                .execute(&pool)
                .await,
            ErrorKind::CheckViolation,
        );
    }
    for status in [
        "received",
        "submitting",
        "completed",
        "failed",
        "outcome_unknown",
    ] {
        sqlx::query("UPDATE actions SET status=$2 WHERE id=$1")
            .bind(id)
            .bind(status)
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn transitions_require_an_existing_action_and_valid_status(pool: PgPool) {
    let id = create_action(
        &pool,
        "dev:local",
        "handler",
        &"01".repeat(20),
        &"02".repeat(32),
    )
    .await
    .unwrap();
    assert_database_error(
        sqlx::query("INSERT INTO action_transitions (action_id, status) VALUES ($1, 'received')")
            .bind(id + 1)
            .execute(&pool)
            .await,
        ErrorKind::ForeignKeyViolation,
    );
    sqlx::query("INSERT INTO action_transitions (action_id, status) VALUES ($1, 'received')")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    for status in ["pending", "invalid"] {
        assert_database_error(
            sqlx::query("INSERT INTO action_transitions (action_id, status) VALUES ($1, $2)")
                .bind(id)
                .bind(status)
                .execute(&pool)
                .await,
            ErrorKind::CheckViolation,
        );
    }
}
