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
    request(app, "POST", "/v1/actions", Some(body), key).await
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    payload: Option<&Value>,
    key: Option<&str>,
) -> Response {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    app.clone()
        .oneshot(
            request
                .body(payload.map_or_else(Body::empty, |value| Body::from(value.to_string())))
                .unwrap(),
        )
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

async fn new_action(app: &Router, event: u8) -> i64 {
    let mut input = action_request();
    input["event_id"] = json!(format!("{event:02x}").repeat(20));
    let response = post(app, &input, Some(KEY)).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    body(response).await["id"].as_i64().unwrap()
}

async fn update_action(app: &Router, id: i64, operation: &str, input: Option<&Value>) -> Response {
    request(
        app,
        "POST",
        &format!("/v1/actions/{id}/{operation}"),
        input,
        Some(KEY),
    )
    .await
}

async fn details(app: &Router, id: i64) -> Value {
    let response = request(app, "GET", &format!("/v1/actions/{id}"), None, Some(KEY)).await;
    assert_eq!(response.status(), StatusCode::OK);
    body(response).await
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn submission_and_result_reports_are_atomic_and_idempotent(pool: PgPool) {
    let app = app(&pool, "dev:local", "handler");
    let id = new_action(&app, 1).await;
    let result = json!({"status": "completed", "tx_hash": "ab".repeat(32)});
    assert_eq!(
        update_action(&app, id, "result", Some(&result))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    // Finish the concurrent submission reports before sending concurrent results.
    for operation in ["submission", "result"] {
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let app = app.clone();
            let input = (operation == "result").then(|| result.clone());
            tasks.push(tokio::spawn(async move {
                let response = update_action(&app, id, operation, input.as_ref()).await;
                assert_eq!(response.status(), StatusCode::OK);
                body(response).await
            }));
        }
        let mut responses = Vec::new();
        for task in tasks {
            responses.push(task.await.unwrap());
        }
        // Duplicate reports must return the same record, including timestamps.
        assert!(responses.iter().all(|response| response == &responses[0]));
    }
    let value = details(&app, id).await;
    assert_eq!(value["action"]["status"], "completed");
    assert_eq!(value["action"]["tx_hash"], "AB".repeat(32));
    assert!(value["action"]["submitted_at"].is_string());
    assert!(value["action"]["last_checked_at"].is_null());
    let history = value["history"].as_array().unwrap();
    assert_eq!(
        history
            .iter()
            .map(|row| row["status"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["received", "submitting", "completed"]
    );
    assert!(history
        .windows(2)
        .all(|rows| rows[0]["id"].as_i64() < rows[1]["id"].as_i64()));
    assert_eq!(
        update_action(&app, id, "submission", None).await.status(),
        StatusCode::CONFLICT
    );
    // Completion is terminal: reject a different hash or a delayed error report.
    for conflicting in [
        json!({"status": "completed", "tx_hash": "cd".repeat(32)}),
        json!({"status": "failed", "error_code": "late_failure"}),
        json!({"status": "outcome_unknown", "error_code": "lost_response"}),
    ] {
        assert_eq!(
            update_action(&app, id, "result", Some(&conflicting))
                .await
                .status(),
            StatusCode::CONFLICT
        );
    }
    // Rejected reports must leave both the action and its history unchanged.
    assert_eq!(details(&app, id).await, value);
    assert_counts(&pool, 1, 3).await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn unknown_results_can_resolve_but_failed_actions_cannot_restart(pool: PgPool) {
    let app = app(&pool, "dev:local", "handler");
    for (event, outcome) in [(1, "completed"), (2, "failed")] {
        let id = new_action(&app, event).await;
        assert_eq!(
            update_action(&app, id, "submission", None).await.status(),
            StatusCode::OK
        );
        let unknown = json!({"status": "outcome_unknown", "error_code": "submission_timeout", "error_detail": "Confirmation timed out."});
        let response = update_action(&app, id, "result", Some(&unknown)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let saved = body(response).await;
        assert_eq!(saved["error_detail"], "Confirmation timed out.");
        let snapshot = details(&app, id).await;
        assert_eq!(
            snapshot["history"][2]["error_detail"],
            "Confirmation timed out."
        );
        let repeated = update_action(&app, id, "result", Some(&unknown)).await;
        assert_eq!(body(repeated).await, saved);
        // A later report can supply a hash while the outcome remains unknown.
        let with_hash = json!({"status": "outcome_unknown", "error_code": "submission_timeout", "error_detail": "Confirmation timed out.", "tx_hash": "ab".repeat(32)});
        assert_eq!(
            update_action(&app, id, "result", Some(&with_hash))
                .await
                .status(),
            StatusCode::OK
        );
        let mut final_result = json!({"status": outcome});
        if outcome == "completed" {
            final_result["tx_hash"] = json!("ab".repeat(32));
        } else {
            final_result["error_code"] = json!("transaction_failed");
        }
        let response = update_action(&app, id, "result", Some(&final_result)).await;
        assert_eq!(response.status(), StatusCode::OK);
        // Resolving the outcome must preserve a known hash even when omitted.
        assert_eq!(body(response).await["tx_hash"], "AB".repeat(32));
        assert_eq!(
            update_action(&app, id, "result", Some(&final_result))
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            update_action(&app, id, "submission", None).await.status(),
            StatusCode::CONFLICT
        );
        let value = details(&app, id).await;
        assert_eq!(value["action"]["status"], outcome);
        assert!(value["action"]["error_detail"].is_null());
        assert!(value["history"][4]["error_detail"].is_null());
        assert_eq!(value["history"].as_array().unwrap().len(), 5);
    }
    let id = new_action(&app, 3).await;
    update_action(&app, id, "submission", None).await;
    let failed = json!({"status": "failed", "error_code": "submission_rejected", "error_detail": "x".repeat(1024)});
    assert_eq!(
        update_action(&app, id, "result", Some(&failed))
            .await
            .status(),
        StatusCode::OK
    );
    let failure = details(&app, id).await;
    assert_eq!(failure["action"]["error_detail"], "x".repeat(1024));
    assert_eq!(failure["history"][2]["error_detail"], "x".repeat(1024));
    assert_eq!(
        update_action(&app, id, "result", Some(&failed))
            .await
            .status(),
        StatusCode::OK
    );
    let changed_detail = json!({"status": "failed", "error_code": "submission_rejected", "error_detail": "Different reason."});
    assert_eq!(
        update_action(&app, id, "result", Some(&changed_detail))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let success = json!({"status": "completed", "tx_hash": "ab".repeat(32)});
    assert_eq!(
        update_action(&app, id, "result", Some(&success))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(details(&app, id).await["action"]["status"], "failed");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn failed_transition_history_write_rolls_back_submission_and_result(pool: PgPool) {
    let app = app(&pool, "dev:local", "handler");
    let id = new_action(&app, 1).await;
    for operation in ["submission", "result"] {
        let before = details(&app, id).await;
        // Keep existing history, but force new history inserts to fail after the action update.
        sqlx::query("ALTER TABLE action_transitions ADD CONSTRAINT reject_updates CHECK (status='received') NOT VALID")
            .execute(&pool).await.unwrap();
        let result = json!({"status": "completed", "tx_hash": "ab".repeat(32)});
        let input = (operation == "result").then_some(&result);
        let response = update_action(&app, id, operation, input).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(details(&app, id).await, before);
        sqlx::query("ALTER TABLE action_transitions DROP CONSTRAINT reject_updates")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            update_action(&app, id, operation, input).await.status(),
            StatusCode::OK
        );
    }
    assert_counts(&pool, 1, 3).await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn action_queries_use_stable_pagination_and_isolate_users_and_deployments(pool: PgPool) {
    let local = app(&pool, "dev:local", "handler");
    let first = new_action(&local, 1).await;
    let second = new_action(&local, 2).await;
    let third = new_action(&local, 3).await;
    let mut other_user = action_request();
    other_user["event_id"] = json!("04".repeat(20));
    other_user["payload"]["email"]["from"] = json!("03".repeat(32));
    assert_eq!(
        post(&local, &other_user, Some(KEY)).await.status(),
        StatusCode::CREATED
    );
    // Knowing an action ID must not bypass the configured chain and handler scope.
    for other in [
        app(&pool, "dev:other", "handler"),
        app(&pool, "dev:local", "other-handler"),
    ] {
        new_action(&other, 1).await;
        for (method, suffix, input) in [
            ("GET", "", None),
            ("POST", "/submission", None),
            (
                "POST",
                "/result",
                Some(json!({"status": "failed", "error_code": "rejected"})),
            ),
        ] {
            let response = request(
                &other,
                method,
                &format!("/v1/actions/{first}{suffix}"),
                input.as_ref(),
                Some(KEY),
            )
            .await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }
    let path = format!("/v1/actions?user_id={}&limit=2", "02".repeat(32));
    let response = request(&local, "GET", &path, None, Some(KEY)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = body(response).await;
    assert_eq!(
        page["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [third, second]
    );
    assert_eq!(page["next_before_id"], second);
    // A newer action inserted between pages must not shift the cursor.
    new_action(&local, 5).await;
    let response = request(
        &local,
        "GET",
        &format!("{path}&before_id={second}"),
        None,
        Some(KEY),
    )
    .await;
    let page = body(response).await;
    assert_eq!(page["actions"].as_array().unwrap().len(), 1);
    assert_eq!(page["actions"][0]["id"], first);
    assert!(page["next_before_id"].is_null());
    let response = request(
        &local,
        "GET",
        &format!("/v1/actions?user_id={}", "ff".repeat(32)),
        None,
        Some(KEY),
    )
    .await;
    assert_eq!(
        body(response).await,
        json!({"actions": [], "next_before_id": null})
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn action_operations_validate_authentication_paths_queries_and_results(pool: PgPool) {
    let app = app(&pool, "dev:local", "handler");
    let id = new_action(&app, 1).await;
    let success = json!({"status": "completed", "tx_hash": "ab".repeat(32)});
    for (method, path, input) in [
        ("GET", format!("/v1/actions/{id}"), None),
        (
            "GET",
            format!("/v1/actions?user_id={}", "02".repeat(32)),
            None,
        ),
        ("POST", format!("/v1/actions/{id}/submission"), None),
        ("POST", format!("/v1/actions/{id}/result"), Some(&success)),
    ] {
        for key in [None, Some("wrong-key")] {
            assert_eq!(
                request(&app, method, &path, input, key).await.status(),
                StatusCode::UNAUTHORIZED
            );
        }
    }
    for path in [
        "/v1/actions/0",
        "/v1/actions/-1/submission",
        "/v1/actions/not-an-id",
        "/v1/actions?user_id=invalid",
        "/v1/actions?limit=2",
    ] {
        let method = if path.ends_with("submission") {
            "POST"
        } else {
            "GET"
        };
        assert_eq!(
            request(&app, method, path, None, Some(KEY)).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    for query in [
        "limit=0",
        "limit=101",
        "limit=-1",
        "before_id=0",
        "before_id=-1",
        "extra=field",
    ] {
        let path = format!("/v1/actions?user_id={}&{query}", "02".repeat(32));
        assert_eq!(
            request(&app, "GET", &path, None, Some(KEY)).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    for (method, suffix, input) in [
        ("GET", "", None),
        ("POST", "/submission", None),
        ("POST", "/result", Some(&success)),
    ] {
        assert_eq!(
            request(
                &app,
                method,
                &format!("/v1/actions/999999{suffix}"),
                input,
                Some(KEY)
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
    }
    update_action(&app, id, "submission", None).await;
    for invalid in [
        json!({"status": "completed"}),
        json!({"status": "completed", "tx_hash": "ab".repeat(32), "error_code": "unexpected"}),
        json!({"status": "failed"}),
        json!({"status": "outcome_unknown", "error_code": ""}),
        json!({"status": "failed", "error_code": "a".repeat(65)}),
        json!({"status": "failed", "error_code": "raw error details"}),
        json!({"status": "failed", "error_code": "rejected", "error_detail": ""}),
        json!({"status": "failed", "error_code": "rejected", "error_detail": "  "}),
        json!({"status": "failed", "error_code": "rejected", "error_detail": "x".repeat(1025)}),
        json!({"status": "failed", "error_code": "rejected", "error_detail": "line\nbreak"}),
        json!({"status": "completed", "tx_hash": "ab".repeat(32), "error_detail": "Unexpected detail"}),
        json!({"status": "completed", "tx_hash": "short"}),
        json!({"status": "completed", "tx_hash": "gg".repeat(32)}),
    ] {
        assert_eq!(
            update_action(&app, id, "result", Some(&invalid))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    for invalid in [
        json!({"status": "pending"}),
        json!({"status": "submitting"}),
        json!({"status": "completed", "tx_hash": "ab".repeat(32), "extra": true}),
    ] {
        assert_eq!(
            update_action(&app, id, "result", Some(&invalid))
                .await
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_counts(&pool, 1, 2).await;
}
