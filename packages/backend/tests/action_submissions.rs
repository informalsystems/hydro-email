//! Database schema tests for action submission tracking.
use sqlx::{error::ErrorKind, PgPool};

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
async fn event_uniqueness_is_scoped_to_chain_and_handler(pool: PgPool) {
    let event_id = "01".repeat(20);
    let user_id = "02".repeat(32);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let pool = pool.clone();
        let event_id = event_id.clone();
        let user_id = user_id.clone();
        tasks.push(tokio::spawn(async move {
            create_action(&pool, "dev:local", "handler", &event_id, &user_id).await
        }));
    }
    let mut created = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => created += 1,
            result => assert_database_error(result, ErrorKind::UniqueViolation),
        }
    }
    assert_eq!(created, 1);

    // One user can have multiple events, and deployments have separate namespaces.
    for (chain, handler, event) in [
        ("dev:other", "handler", event_id.clone()),
        ("dev:local", "other-handler", event_id),
        ("dev:local", "handler", "03".repeat(20)),
    ] {
        let id = create_action(&pool, chain, handler, &event, &user_id)
            .await
            .unwrap();
        let status: String = sqlx::query_scalar("SELECT status FROM actions WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "received");
    }
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
    assert_database_error(
        sqlx::query("UPDATE actions SET status='invalid' WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await,
        ErrorKind::CheckViolation,
    );
    for status in [
        "received",
        "submitting",
        "pending",
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
async fn attempts_require_valid_numbers_and_pending_transaction_hashes(pool: PgPool) {
    let id = create_action(
        &pool,
        "dev:local",
        "handler",
        &"01".repeat(20),
        &"02".repeat(32),
    )
    .await
    .unwrap();
    for number in [0, -1] {
        assert_database_error(
            sqlx::query("INSERT INTO action_submission_attempts (action_id, attempt_number) VALUES ($1, $2)")
                .bind(id).bind(number as i64).execute(&pool).await,
            ErrorKind::CheckViolation,
        );
    }
    sqlx::query(
        "INSERT INTO action_submission_attempts (action_id, attempt_number) VALUES ($1, 1)",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    assert_database_error(
        sqlx::query(
            "INSERT INTO action_submission_attempts (action_id, attempt_number) VALUES ($1, 1)",
        )
        .bind(id)
        .execute(&pool)
        .await,
        ErrorKind::UniqueViolation,
    );
    for status in ["pending", "received", "invalid"] {
        assert_database_error(
            sqlx::query("UPDATE action_submission_attempts SET status=$2 WHERE action_id=$1")
                .bind(id)
                .bind(status)
                .execute(&pool)
                .await,
            ErrorKind::CheckViolation,
        );
    }
    // Uncertainty does not require a hash, but pending inclusion does.
    sqlx::query(
        "UPDATE action_submission_attempts SET status='outcome_unknown' WHERE action_id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE action_submission_attempts SET status='pending', tx_hash='test-hash' WHERE action_id=$1")
        .bind(id).execute(&pool).await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn transitions_reference_the_correct_action_attempt(pool: PgPool) {
    let first = create_action(
        &pool,
        "dev:local",
        "handler",
        &"01".repeat(20),
        &"02".repeat(32),
    )
    .await
    .unwrap();
    let second = create_action(
        &pool,
        "dev:local",
        "handler",
        &"03".repeat(20),
        &"02".repeat(32),
    )
    .await
    .unwrap();
    // Initial history can be recorded before any submission attempt exists.
    sqlx::query("INSERT INTO action_transitions (action_id, status) VALUES ($1, 'received')")
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO action_submission_attempts (action_id, attempt_number) VALUES ($1, 1)",
    )
    .bind(first)
    .execute(&pool)
    .await
    .unwrap();
    assert_database_error(
        sqlx::query("INSERT INTO action_transitions (action_id, attempt_number, status) VALUES ($1, 1, 'submitting')")
            .bind(second).execute(&pool).await,
        ErrorKind::ForeignKeyViolation,
    );
    sqlx::query("INSERT INTO action_transitions (action_id, attempt_number, status) VALUES ($1, 1, 'submitting')")
        .bind(first).execute(&pool).await.unwrap();
    assert_database_error(
        sqlx::query("INSERT INTO action_transitions (action_id, status) VALUES ($1, 'invalid')")
            .bind(first)
            .execute(&pool)
            .await,
        ErrorKind::CheckViolation,
    );
}
