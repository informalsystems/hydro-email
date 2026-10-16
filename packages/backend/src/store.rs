use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};

#[derive(Clone)]
pub struct Store {
    pub pool: PgPool,
    pub chain_key: String,
    pub registry_address: String,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Registration {
    pub user_id: String,
    #[serde(skip_serializing)]
    pub email: String,
    pub status: String,
    pub attempt_number: i64,
    #[serde(skip_serializing)]
    pub submission_stage: Option<String>,
    pub proxy_address: Option<String>,
    pub create_tx_hash: Option<String>,
    pub register_tx_hash: Option<String>,
    pub error_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub enum Admission {
    Created(Registration),
    Resumed(Registration),
    Existing(Registration),
    QuotaExceeded,
}

impl Store {
    pub async fn migrate(pool: &PgPool) -> Result<()> {
        sqlx::migrate!().run(pool).await?;
        Ok(())
    }

    pub async fn get(&self, user_id: &str) -> Result<Option<Registration>> {
        Ok(sqlx::query_as(
            "SELECT * FROM registrations WHERE chain_key=$1 AND registry_address=$2 AND user_id=$3",
        )
        .bind(&self.chain_key)
        .bind(&self.registry_address)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn admit(
        &self,
        email: &str,
        user_id: &str,
        hourly: i64,
        daily: i64,
    ) -> Result<Admission> {
        let mut tx = self.pool.begin().await?;
        // Serialize admission across replicas and registries on this chain. A unique
        // constraint alone protects duplicates, but would not enforce a global quota.
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtext('hydro-email-admission'), hashtext($1))",
        )
        .bind(&self.chain_key)
        .execute(&mut *tx)
        .await?;
        let existing = sqlx::query_as::<_, Registration>(
            "SELECT * FROM registrations WHERE chain_key=$1 AND registry_address=$2 AND user_id=$3 FOR UPDATE",
        )
        .bind(&self.chain_key)
        .bind(&self.registry_address)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(existing) = &existing {
            if !matches!(existing.status.as_str(), "failed" | "unknown") {
                return Ok(Admission::Existing(existing.clone()));
            }
        }
        // Accepted jobs reserve capacity immediately, including failures/unknowns.
        // These are rolling windows, not calendar-day resets.
        let (hour, day): (i64, i64) = sqlx::query_as("SELECT count(*) FILTER (WHERE occurred_at > now() - interval '1 hour'), count(*) FROM registration_transitions WHERE chain_key=$1 AND status='queued' AND occurred_at > now() - interval '24 hours'")
            .bind(&self.chain_key).fetch_one(&mut *tx).await?;
        if hour >= hourly || day >= daily {
            return Ok(Admission::QuotaExceeded);
        }
        if existing.is_some() {
            // Preserve the expected proxy; previous hashes remain in the attempt history.
            // Incrementing the attempt fences out stale worker updates.
            let row: Registration = sqlx::query_as("UPDATE registrations SET status='queued', attempt_number=attempt_number+1, submission_stage=NULL, create_tx_hash=NULL, register_tx_hash=NULL, error_code=NULL, updated_at=now(), next_check_at=now() WHERE chain_key=$1 AND registry_address=$2 AND user_id=$3 RETURNING *")
                .bind(&self.chain_key).bind(&self.registry_address).bind(user_id)
                .fetch_one(&mut *tx).await?;
            self.audit(&mut tx, user_id, row.attempt_number, "queued", None)
                .await?;
            tx.commit().await?;
            return Ok(Admission::Resumed(row));
        }
        let row: Registration = sqlx::query_as("INSERT INTO registrations (chain_key, registry_address, user_id, email) VALUES ($1,$2,$3,$4) RETURNING *")
            .bind(&self.chain_key).bind(&self.registry_address).bind(user_id).bind(email)
            .fetch_one(&mut *tx).await?;
        self.audit(&mut tx, user_id, row.attempt_number, "queued", None)
            .await?;
        tx.commit().await?;
        Ok(Admission::Created(row))
    }

    async fn audit(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        user_id: &str,
        attempt_number: i64,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query("INSERT INTO registration_transitions (chain_key, registry_address, user_id, attempt_number, status, error_code) VALUES ($1,$2,$3,$4,$5,$6)")
            .bind(&self.chain_key).bind(&self.registry_address).bind(user_id).bind(attempt_number).bind(status).bind(error)
            .execute(&mut **tx).await?;
        Ok(())
    }

    pub async fn transition(
        &self,
        job: &Registration,
        status: &str,
        proxy: Option<&str>,
        create_hash: Option<&str>,
        register_hash: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let changed = sqlx::query("UPDATE registrations SET status=$4, proxy_address=COALESCE($5,proxy_address), create_tx_hash=COALESCE($6,create_tx_hash), register_tx_hash=COALESCE($7,register_tx_hash), error_code=$8, updated_at=now(), next_check_at=now() WHERE chain_key=$1 AND registry_address=$2 AND user_id=$3 AND attempt_number=$9")
            .bind(&self.chain_key).bind(&self.registry_address).bind(&job.user_id).bind(status)
            .bind(proxy).bind(create_hash).bind(register_hash).bind(error).bind(job.attempt_number)
            .execute(&mut *tx).await?.rows_affected();
        if changed == 0 {
            return Ok(());
        }
        if let Some(hash) = create_hash.or(register_hash) {
            let stage = if create_hash.is_some() {
                "creating_proxy"
            } else {
                "registering"
            };
            sqlx::query("UPDATE registration_attempts SET tx_hash=$6 WHERE chain_key=$1 AND registry_address=$2 AND user_id=$3 AND attempt_number=$4 AND stage=$5")
                .bind(&self.chain_key).bind(&self.registry_address).bind(&job.user_id)
                .bind(job.attempt_number).bind(stage).bind(hash).execute(&mut *tx).await?;
        }
        self.audit(&mut tx, &job.user_id, job.attempt_number, status, error)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn next(&self) -> Result<Option<Registration>> {
        // A single elected worker processes this namespace. Uncertain attempts
        // take priority and block further wallet use until chain evidence resolves them.
        Ok(sqlx::query_as("SELECT * FROM registrations WHERE chain_key=$1 AND registry_address=$2 AND status NOT IN ('completed','failed') AND next_check_at <= now() ORDER BY CASE WHEN status IN ('creating_proxy','registering','proxy_pending','registration_pending','unknown') THEN 0 ELSE 1 END, created_at, user_id LIMIT 1")
            .bind(&self.chain_key).bind(&self.registry_address).fetch_optional(&self.pool).await?)
    }

    pub async fn begin_attempt(
        &self,
        job: &Registration,
        expected: &str,
        stage: &str,
        proxy: &str,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let changed = sqlx::query("UPDATE registrations SET status=$4, submission_stage=$4, proxy_address=$7, create_tx_hash=CASE WHEN $4='creating_proxy' THEN NULL ELSE create_tx_hash END, register_tx_hash=NULL, updated_at=now(), error_code=NULL WHERE chain_key=$1 AND registry_address=$2 AND user_id=$3 AND status=$5 AND attempt_number=$6")
            .bind(&self.chain_key).bind(&self.registry_address).bind(&job.user_id).bind(stage).bind(expected).bind(job.attempt_number).bind(proxy)
            .execute(&mut *tx).await?.rows_affected();
        if changed == 0 {
            return Ok(false);
        }
        // Keep a durable attempt record independently of the current status so
        // restoring a job's status cannot accidentally allow a second broadcast.
        let inserted = sqlx::query("INSERT INTO registration_attempts (chain_key, registry_address, user_id, attempt_number, stage) VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
            .bind(&self.chain_key).bind(&self.registry_address).bind(&job.user_id).bind(job.attempt_number).bind(stage)
            .execute(&mut *tx).await?.rows_affected();
        if inserted == 0 {
            return Ok(false);
        }
        self.audit(&mut tx, &job.user_id, job.attempt_number, stage, None)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn defer(&self, user_id: &str) -> Result<()> {
        sqlx::query("UPDATE registrations SET next_check_at=now() + interval '5 seconds' WHERE chain_key=$1 AND registry_address=$2 AND user_id=$3")
            .bind(&self.chain_key).bind(&self.registry_address).bind(user_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn has_uncertain(&self) -> Result<bool> {
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM registrations WHERE chain_key=$1 AND registry_address=$2 AND status IN ('creating_proxy','registering','proxy_pending','registration_pending','unknown'))")
            .bind(&self.chain_key).bind(&self.registry_address).fetch_one(&self.pool).await?)
    }
}
