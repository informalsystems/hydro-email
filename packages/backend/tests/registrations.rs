//! Registration API, persistence, and worker tests.
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use anyhow::Result;
use app_backend::{
    action_store::ActionStore,
    api::{router, ApiState},
    chain::{Broadcast, Confirmation, ProxyState, RegistrationChain},
    registration_store::{Admission, RegistrationStore},
    worker::Worker,
};
use app_contract_api::user_registry::msg::UserId;
use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower::ServiceExt;

fn store(pool: PgPool) -> RegistrationStore {
    RegistrationStore {
        pool,
        chain_key: "dev:local".into(),
        registry_address: "test-registry".into(),
    }
}

async fn admit(store: &RegistrationStore, email: &str) -> String {
    let id = UserId::new_email_address(email).to_string();
    assert!(matches!(
        store.admit(email, &id, 100, 100).await.unwrap(),
        Admission::Created(_)
    ));
    id
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn concurrent_domain_case_variants_reserve_one_registration(pool: PgPool) {
    let store = store(pool);
    let mut tasks = Vec::new();
    for email in [
        "alice@example.com",
        "alice@Example.com",
        "alice@EXAMPLE.COM",
        "alice@eXaMpLe.CoM",
    ] {
        let store = store.clone();
        let id = UserId::new_email_address(email).to_string();
        tasks.push(tokio::spawn(async move {
            store.admit(email, &id, 1, 1).await.unwrap()
        }));
    }
    let mut created = 0;
    for task in tasks {
        match task.await.unwrap() {
            Admission::Created(_) => created += 1,
            Admission::Existing(_) => {}
            Admission::Resumed(_) => panic!("A new registration must not be resumed"),
            Admission::QuotaExceeded => {
                panic!("Duplicates must return their existing registration even at quota")
            }
        }
    }
    assert_eq!(created, 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM registration_transitions")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn quotas_are_atomic_across_registries_but_chain_scoped(pool: PgPool) {
    let store = store(pool);
    let mut tasks = Vec::new();
    for n in 0..12 {
        let mut store = store.clone();
        store.registry_address = format!("registry-{n}");
        tasks.push(tokio::spawn(async move {
            let email = format!("user{n}@example.com");
            store
                .admit(&email, UserId::new_email_address(&email).as_str(), 3, 10)
                .await
                .unwrap()
        }));
    }
    let mut accepted = 0;
    for task in tasks {
        if matches!(task.await.unwrap(), Admission::Created(_)) {
            accepted += 1;
        }
    }
    assert_eq!(accepted, 3);
    let mut other = store.clone();
    other.chain_key = "cosmos:test".into();
    assert!(matches!(
        other
            .admit(
                "user@example.com",
                UserId::new_email_address("user@example.com").as_str(),
                1,
                1
            )
            .await
            .unwrap(),
        Admission::Created(_)
    ));
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn concurrent_attempts_and_status_resets_cannot_resubmit(pool: PgPool) {
    let store = store(pool);
    let first = admit(&store, "alice@example.com").await;
    let job = store.get(&first).await.unwrap().unwrap();
    let (a, b) = tokio::join!(
        store.begin_attempt(&job, "queued", "creating_proxy", "proxy-address"),
        store.begin_attempt(&job, "queued", "creating_proxy", "proxy-address"),
    );
    assert_ne!(a.unwrap(), b.unwrap());
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM registration_attempts")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(total, 1);
    let attempted = &first;
    // Even accidental/manual state restoration cannot permit a second attempt.
    store
        .transition(&job, "queued", None, None, None, None)
        .await
        .unwrap();
    assert!(!store
        .begin_attempt(&job, "queued", "creating_proxy", "proxy-address")
        .await
        .unwrap());
    assert_eq!(
        store.get(attempted).await.unwrap().unwrap().status,
        "queued"
    );
}

#[derive(Default)]
struct FakeChain {
    creates: AtomicUsize,
    registers: AtomicUsize,
    registered: AtomicBool,
    proxy_deployed: AtomicBool,
    create_times_out: AtomicBool,
    create_lands_on_timeout: bool,
    register_rejected: AtomicBool,
    invalid_proxy: bool,
    register_times_out: bool,
    confirmation_pending: bool,
    transaction_fails: AtomicBool,
    lookup_fails: bool,
}

#[async_trait]
impl RegistrationChain for FakeChain {
    async fn lookup(&self, _id: &str) -> Result<Option<String>> {
        if self.lookup_fails {
            anyhow::bail!("RPC unavailable");
        }
        Ok(self
            .registered
            .load(Ordering::SeqCst)
            .then(|| "proxy-address".into()))
    }
    async fn proxy_address(&self, _id: &str) -> Result<String> {
        Ok("proxy-address".into())
    }
    async fn proxy_state(&self, _address: &str) -> Result<ProxyState> {
        if self.invalid_proxy {
            return Ok(ProxyState::Invalid);
        }
        Ok(if self.proxy_deployed.load(Ordering::SeqCst) {
            ProxyState::Ready
        } else {
            ProxyState::Missing
        })
    }
    async fn create_proxy(&self, _id: &str) -> Result<Broadcast> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        if self.create_times_out.load(Ordering::SeqCst) {
            self.proxy_deployed
                .store(self.create_lands_on_timeout, Ordering::SeqCst);
            anyhow::bail!("ambiguous network failure");
        }
        if !self.transaction_fails.load(Ordering::SeqCst) && !self.confirmation_pending {
            self.proxy_deployed.store(true, Ordering::SeqCst);
        }
        Ok(Broadcast::Accepted("create-hash".into()))
    }
    async fn register(&self, _id: &str, _proxy: &str) -> Result<Broadcast> {
        self.registers.fetch_add(1, Ordering::SeqCst);
        if self.register_rejected.load(Ordering::SeqCst) {
            return Ok(Broadcast::Rejected(anyhow::anyhow!(
                "registration rejected"
            )));
        }
        self.registered.store(true, Ordering::SeqCst);
        if self.register_times_out {
            anyhow::bail!("broadcast response lost");
        }
        Ok(Broadcast::Accepted("register-hash".into()))
    }
    async fn confirm(&self, _hash: &str) -> Result<Confirmation> {
        if self.confirmation_pending {
            return Ok(Confirmation::Pending);
        }
        if self.transaction_fails.load(Ordering::SeqCst) {
            return Ok(Confirmation::Failed);
        }
        Ok(Confirmation::Succeeded)
    }
}

fn worker(store: RegistrationStore, chain: Arc<FakeChain>) -> Worker {
    Worker { store, chain }
}

async fn step(worker: &Worker, id: &str) -> String {
    let job = worker.store.get(id).await.unwrap().unwrap();
    worker.step(&job).await.unwrap();
    worker.store.get(id).await.unwrap().unwrap().status
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn provisions_once_and_persists_each_step(pool: PgPool) {
    let store = store(pool);
    let id = admit(&store, "alice@example.com").await;
    let chain = Arc::new(FakeChain::default());
    let worker = worker(store, chain.clone());
    assert_eq!(step(&worker, &id).await, "proxy_pending");
    assert_eq!(step(&worker, &id).await, "proxy_created");
    assert_eq!(step(&worker, &id).await, "registration_pending");
    assert_eq!(step(&worker, &id).await, "completed");
    assert_eq!(step(&worker, &id).await, "completed");
    assert_eq!(chain.creates.load(Ordering::SeqCst), 1);
    assert_eq!(chain.registers.load(Ordering::SeqCst), 1);
    let record = worker.store.get(&id).await.unwrap().unwrap();
    assert_eq!(record.proxy_address.as_deref(), Some("proxy-address"));
    assert_eq!(record.create_tx_hash.as_deref(), Some("create-hash"));
    assert_eq!(record.register_tx_hash.as_deref(), Some("register-hash"));
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn timeout_or_crash_never_resubmits_and_blocks_new_wallet_use(pool: PgPool) {
    let store = store(pool);
    let id = admit(&store, "alice@example.com").await;
    let second = admit(&store, "bob@example.com").await;
    let chain = Arc::new(FakeChain {
        create_times_out: AtomicBool::new(true),
        ..Default::default()
    });
    let worker = worker(store, chain.clone());
    assert_eq!(step(&worker, &id).await, "unknown");
    assert_eq!(step(&worker, &id).await, "unknown");
    assert_eq!(step(&worker, &second).await, "queued");
    assert_eq!(chain.creates.load(Ordering::SeqCst), 1);
    // Simulate a restart after intent was recorded but before a hash was saved.
    worker
        .store
        .transition(
            &worker.store.get(&second).await.unwrap().unwrap(),
            "creating_proxy",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(step(&worker, &second).await, "unknown");
    assert_eq!(chain.creates.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn chain_failure_waits_for_a_new_registration_request(pool: PgPool) {
    let store = store(pool);
    let id = admit(&store, "alice@example.com").await;
    let chain = Arc::new(FakeChain {
        transaction_fails: AtomicBool::new(true),
        ..Default::default()
    });
    let worker = worker(store, chain.clone());
    assert_eq!(step(&worker, &id).await, "proxy_pending");
    assert_eq!(step(&worker, &id).await, "failed");
    assert_eq!(step(&worker, &id).await, "failed");
    assert_eq!(chain.creates.load(Ordering::SeqCst), 1);
    assert_eq!(chain.registers.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn failed_creation_can_be_retried_and_completed(pool: PgPool) {
    let store = store(pool);
    let id = admit(&store, "alice@example.com").await;
    let chain = Arc::new(FakeChain {
        transaction_fails: AtomicBool::new(true),
        ..Default::default()
    });
    let worker = worker(store, chain.clone());
    assert_eq!(step(&worker, &id).await, "proxy_pending");
    assert_eq!(step(&worker, &id).await, "failed");
    chain.transaction_fails.store(false, Ordering::SeqCst);
    assert!(matches!(
        worker
            .store
            .admit("alice@example.com", &id, 100, 100)
            .await
            .unwrap(),
        Admission::Resumed(_)
    ));
    assert_eq!(step(&worker, &id).await, "proxy_pending");
    assert_eq!(step(&worker, &id).await, "proxy_created");
    assert_eq!(step(&worker, &id).await, "registration_pending");
    assert_eq!(step(&worker, &id).await, "completed");
    assert_eq!(chain.creates.load(Ordering::SeqCst), 2);
    assert_eq!(
        worker.store.get(&id).await.unwrap().unwrap().attempt_number,
        2
    );
    let hashes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM registration_attempts WHERE tx_hash IS NOT NULL")
            .fetch_one(&worker.store.pool)
            .await
            .unwrap();
    assert_eq!(hashes, 3);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn retry_reuses_a_proxy_when_only_registry_registration_failed(pool: PgPool) {
    let store = store(pool);
    let id = admit(&store, "alice@example.com").await;
    let chain = Arc::new(FakeChain {
        register_rejected: AtomicBool::new(true),
        ..Default::default()
    });
    let worker = worker(store, chain.clone());
    assert_eq!(step(&worker, &id).await, "proxy_pending");
    assert_eq!(step(&worker, &id).await, "proxy_created");
    assert_eq!(step(&worker, &id).await, "failed");
    chain.register_rejected.store(false, Ordering::SeqCst);
    assert!(matches!(
        worker
            .store
            .admit("alice@example.com", &id, 100, 100)
            .await
            .unwrap(),
        Admission::Resumed(_)
    ));
    assert_eq!(step(&worker, &id).await, "proxy_created");
    assert_eq!(step(&worker, &id).await, "registration_pending");
    assert_eq!(step(&worker, &id).await, "completed");
    assert_eq!(chain.creates.load(Ordering::SeqCst), 1);
    assert_eq!(chain.registers.load(Ordering::SeqCst), 2);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn unknown_creation_recovers_an_existing_proxy_without_its_hash(pool: PgPool) {
    let store = store(pool);
    let id = admit(&store, "alice@example.com").await;
    let chain = Arc::new(FakeChain {
        create_times_out: AtomicBool::new(true),
        create_lands_on_timeout: true,
        ..Default::default()
    });
    let worker = worker(store, chain.clone());
    assert_eq!(step(&worker, &id).await, "unknown");
    assert_eq!(step(&worker, &id).await, "proxy_created");
    assert_eq!(step(&worker, &id).await, "registration_pending");
    assert_eq!(step(&worker, &id).await, "completed");
    assert_eq!(chain.creates.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn concurrent_retries_resume_once_and_stale_updates_cannot_overwrite_them(pool: PgPool) {
    let store = store(pool);
    let id = admit(&store, "alice@example.com").await;
    let chain = Arc::new(FakeChain {
        create_times_out: AtomicBool::new(true),
        ..Default::default()
    });
    let worker = worker(store, chain.clone());
    assert_eq!(step(&worker, &id).await, "unknown");
    let stale = worker.store.get(&id).await.unwrap().unwrap();
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = worker.store.clone();
        let id = id.clone();
        tasks.push(tokio::spawn(async move {
            store
                .admit("alice@example.com", &id, 100, 100)
                .await
                .unwrap()
        }));
    }
    let mut resumed = 0;
    for task in tasks {
        if matches!(task.await.unwrap(), Admission::Resumed(_)) {
            resumed += 1;
        }
    }
    assert_eq!(resumed, 1);
    worker
        .store
        .transition(&stale, "failed", None, None, None, Some("stale_failure"))
        .await
        .unwrap();
    let job = worker.store.get(&id).await.unwrap().unwrap();
    assert_eq!(job.status, "queued");
    assert_eq!(job.attempt_number, 2);
    chain.create_times_out.store(false, Ordering::SeqCst);
    assert_eq!(step(&worker, &id).await, "proxy_pending");
    assert_eq!(step(&worker, &id).await, "proxy_created");
    assert_eq!(step(&worker, &id).await, "registration_pending");
    assert_eq!(step(&worker, &id).await, "completed");
    assert_eq!(chain.creates.load(Ordering::SeqCst), 2);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn retries_consume_quota_but_active_and_completed_requests_do_not(pool: PgPool) {
    let store = store(pool);
    let id = admit(&store, "alice@example.com").await;
    let job = store.get(&id).await.unwrap().unwrap();
    assert!(matches!(
        store.admit("alice@example.com", &id, 1, 1).await.unwrap(),
        Admission::Existing(_)
    ));
    store
        .transition(
            &job,
            "failed",
            None,
            None,
            None,
            Some("chain_transaction_failed"),
        )
        .await
        .unwrap();
    assert!(matches!(
        store.admit("alice@example.com", &id, 1, 1).await.unwrap(),
        Admission::QuotaExceeded
    ));
    assert!(matches!(
        store.admit("alice@example.com", &id, 2, 2).await.unwrap(),
        Admission::Resumed(_)
    ));
    let current = store.get(&id).await.unwrap().unwrap();
    store
        .transition(
            &current,
            "completed",
            Some("proxy-address"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert!(matches!(
        store.admit("alice@example.com", &id, 1, 1).await.unwrap(),
        Admission::Existing(_)
    ));
    assert_eq!(store.get(&id).await.unwrap().unwrap().attempt_number, 2);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn refuses_existing_proxy_with_unexpected_configuration(pool: PgPool) {
    let store = store(pool);
    let id = admit(&store, "alice@example.com").await;
    let chain = Arc::new(FakeChain {
        invalid_proxy: true,
        ..Default::default()
    });
    let worker = worker(store, chain.clone());
    assert_eq!(step(&worker, &id).await, "failed");
    assert_eq!(chain.creates.load(Ordering::SeqCst), 0);
    assert_eq!(chain.registers.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn failed_lookup_does_not_trigger_creation(pool: PgPool) {
    let store = store(pool);
    let id = admit(&store, "alice@example.com").await;
    let chain = Arc::new(FakeChain {
        lookup_fails: true,
        ..Default::default()
    });
    let worker = worker(store, chain.clone());
    assert_eq!(step(&worker, &id).await, "queued");
    assert_eq!(chain.creates.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn imports_existing_onchain_registration_without_spending(pool: PgPool) {
    let store = store(pool);
    let id = admit(&store, "alice@example.com").await;
    let chain = Arc::new(FakeChain {
        registered: AtomicBool::new(true),
        ..Default::default()
    });
    let worker = worker(store, chain.clone());
    assert_eq!(step(&worker, &id).await, "completed");
    assert_eq!(chain.creates.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn lost_registration_response_reconciles_without_rebroadcast(pool: PgPool) {
    let store = store(pool);
    let id = admit(&store, "alice@example.com").await;
    let chain = Arc::new(FakeChain {
        register_times_out: true,
        ..Default::default()
    });
    let worker = worker(store, chain.clone());
    assert_eq!(step(&worker, &id).await, "proxy_pending");
    assert_eq!(step(&worker, &id).await, "proxy_created");
    assert_eq!(step(&worker, &id).await, "unknown");
    assert_eq!(step(&worker, &id).await, "completed");
    assert_eq!(chain.registers.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn known_hash_stays_pending_without_a_second_attempt(pool: PgPool) {
    let store = store(pool);
    let id = admit(&store, "alice@example.com").await;
    let chain = Arc::new(FakeChain {
        confirmation_pending: true,
        ..Default::default()
    });
    let worker = worker(store, chain.clone());
    assert_eq!(step(&worker, &id).await, "proxy_pending");
    assert_eq!(step(&worker, &id).await, "proxy_pending");
    assert_eq!(step(&worker, &id).await, "proxy_pending");
    assert_eq!(chain.creates.load(Ordering::SeqCst), 1);
    assert_eq!(chain.registers.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires isolated Postgres"]
async fn http_authentication_validation_and_idempotency(pool: PgPool) {
    let key = "test-credential-at-least-32-characters";
    let app = router(ApiState {
        store: store(pool.clone()),
        actions: ActionStore {
            pool,
            chain_key: "dev:local".into(),
            service_handler_address: "handler".into(),
        },
        api_key_hash: Sha256::digest(key).into(),
        hourly_quota: 1,
        daily_quota: 1,
    });
    let request = |body: &str, token: Option<&str>| {
        let mut request =
            Request::post("/v1/registrations").header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        request.body(Body::from(body.to_owned())).unwrap()
    };
    for token in [None, Some("wrong-key")] {
        let response = app
            .clone()
            .oneshot(request(r#"{"email":"alice@example.com"}"#, token))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let response = app
        .clone()
        .oneshot(request(r#"{"email":"invalid"}"#, Some(key)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app
        .clone()
        .oneshot(request(
            r#"{"email":"alice@example.com","proxy_address":"attacker"}"#,
            Some(key),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let response = app
        .clone()
        .oneshot(request(r#"{"email":"alice@example.com"}"#, Some(key)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert!(body.get("email").is_none());
    assert_eq!(body["status"], "queued");
    let id = body["user_id"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(request(r#"{"email":"alice@EXAMPLE.COM"}"#, Some(key)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // A distinct local-part case must be treated as a new registration, not an alias.
    let response = app
        .clone()
        .oneshot(request(r#"{"email":"Alice@example.com"}"#, Some(key)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let response = app
        .clone()
        .oneshot(request(r#"{"email":"bob@example.com"}"#, Some(key)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let response = app
        .oneshot(
            Request::get(format!("/v1/registrations/{id}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
