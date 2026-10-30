use std::{sync::Arc, time::Duration};

use anyhow::Result;
use sqlx::{Connection, PgConnection};

use crate::{
    chain::{Broadcast, Confirmation, ProxyState, RegistrationChain},
    registration_store::{Registration, RegistrationStore},
};

pub struct Worker {
    pub store: RegistrationStore,
    pub chain: Arc<dyn RegistrationChain>,
}

impl Worker {
    pub async fn run(&self, database_url: &str) -> Result<()> {
        // A dedicated session holds the lock: it must never be returned to a pool.
        // Serialize the entire chain's provisioner, including separate registries,
        // because they may be using the same wallet/account sequence.
        let mut leader = PgConnection::connect(database_url).await?;
        let locked: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_lock(hashtext('hydro-email-provisioner'), hashtext($1))",
        )
        .bind(&self.store.chain_key)
        .fetch_one(&mut leader)
        .await?;
        if !locked {
            anyhow::bail!("Another provisioner already holds this chain's worker lock");
        }
        loop {
            // Stop on loss of the leader's DB session. Never continue submitting
            // while another instance could acquire the released lock.
            sqlx::query("SELECT 1").execute(&mut leader).await?;
            if let Some(job) = self.store.next().await? {
                self.step(&job).await?;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    pub async fn step(&self, job: &Registration) -> Result<()> {
        if matches!(job.status.as_str(), "completed" | "failed") {
            return Ok(());
        }
        if self.reconcile_mapping(job).await? {
            return Ok(());
        }
        match job.status.as_str() {
            "queued" => {
                if self.store.has_uncertain().await? {
                    return Ok(());
                }
                let proxy = match self.chain.proxy_address(&job.user_id).await {
                    Ok(proxy) => proxy,
                    Err(_) => return self.store.defer(&job.user_id).await,
                };
                if job
                    .proxy_address
                    .as_ref()
                    .is_some_and(|saved| saved != &proxy)
                {
                    return self.fail(job, "proxy_configuration_changed").await;
                }
                match self.chain.proxy_state(&proxy).await {
                    Ok(ProxyState::Ready) => {
                        self.store
                            .transition(job, "proxy_created", Some(&proxy), None, None, None)
                            .await?
                    }
                    Ok(ProxyState::Invalid) => {
                        self.fail(job, "proxy_configuration_mismatch").await?
                    }
                    Ok(ProxyState::Missing) => {
                        if self
                            .store
                            .begin_attempt(job, "queued", "creating_proxy", &proxy)
                            .await?
                        {
                            self.record_broadcast(
                                job,
                                true,
                                self.chain.create_proxy(&job.user_id).await,
                            )
                            .await?;
                        }
                    }
                    Err(_) => self.store.defer(&job.user_id).await?,
                }
            }
            "proxy_created" => {
                if self.store.has_uncertain().await? {
                    return Ok(());
                }
                let proxy = job
                    .proxy_address
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("Missing proxy address"))?;
                match self.chain.proxy_state(proxy).await {
                    Ok(ProxyState::Ready) => {
                        if self
                            .store
                            .begin_attempt(job, "proxy_created", "registering", proxy)
                            .await?
                        {
                            self.record_broadcast(
                                job,
                                false,
                                self.chain.register(&job.user_id, proxy).await,
                            )
                            .await?;
                        }
                    }
                    Ok(ProxyState::Invalid) => {
                        self.fail(job, "proxy_configuration_mismatch").await?
                    }
                    _ => self.store.defer(&job.user_id).await?,
                }
            }
            "creating_proxy"
            | "registering"
            | "proxy_pending"
            | "registration_pending"
            | "unknown" => {
                let creating = job.submission_stage.as_deref() == Some("creating_proxy");
                if creating {
                    if let Some(proxy) = &job.proxy_address {
                        match self.chain.proxy_state(proxy).await {
                            Ok(ProxyState::Ready) => {
                                self.store
                                    .transition(job, "proxy_created", None, None, None, None)
                                    .await?;
                                return Ok(());
                            }
                            Ok(ProxyState::Invalid) => {
                                return self.fail(job, "proxy_configuration_mismatch").await
                            }
                            _ => {}
                        }
                    }
                }
                let hash = if creating {
                    &job.create_tx_hash
                } else {
                    &job.register_tx_hash
                };
                if let Some(hash) = hash {
                    if let Ok(Confirmation::Failed) = self.chain.confirm(hash).await {
                        return self.fail(job, "chain_transaction_failed").await;
                    }
                } else if job.status != "unknown" {
                    // A crash or RPC error does not prove failure. A fresh POST
                    // can resume this record, first checking the deterministic
                    // address/mapping again rather than automatically resubmitting.
                    self.store
                        .transition(
                            job,
                            "unknown",
                            None,
                            None,
                            None,
                            Some("submission_outcome_unknown"),
                        )
                        .await?;
                }
                self.store.defer(&job.user_id).await?;
            }
            _ => {}
        }
        Ok(())
    }

    // Returns true when this step is resolved or a read failed and must be deferred.
    async fn reconcile_mapping(&self, job: &Registration) -> Result<bool> {
        match self.chain.lookup(&job.user_id).await {
            Ok(Some(proxy)) => {
                if job
                    .proxy_address
                    .as_ref()
                    .is_some_and(|expected| expected != &proxy)
                {
                    self.fail(job, "registry_mapping_conflict").await?;
                } else {
                    // A registry entry alone does not establish that this is a
                    // safe funding address. Apply the same checks as provisioning.
                    match self.chain.proxy_state(&proxy).await {
                        Ok(ProxyState::Ready) => (),
                        Ok(ProxyState::Invalid | ProxyState::Missing) => {
                            self.fail(job, "proxy_configuration_mismatch").await?;
                            return Ok(true);
                        }
                        Err(_) => {
                            self.store.defer(&job.user_id).await?;
                            return Ok(true);
                        }
                    }
                    self.store
                        .transition(job, "completed", Some(&proxy), None, None, None)
                        .await?;
                    tracing::info!(
                        chain_key = %self.store.chain_key,
                        registry_address = %self.store.registry_address,
                        user_id = %job.user_id,
                        attempt_number = job.attempt_number,
                        proxy_address = %proxy,
                        "Registration confirmed on-chain"
                    );
                }
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(_) => {
                self.store.defer(&job.user_id).await?;
                Ok(true)
            }
        }
    }

    async fn fail(&self, job: &Registration, code: &str) -> Result<()> {
        self.store
            .transition(job, "failed", None, None, None, Some(code))
            .await
    }

    async fn record_broadcast(
        &self,
        job: &Registration,
        creating: bool,
        outcome: Result<Broadcast>,
    ) -> Result<()> {
        match outcome {
            Ok(Broadcast::Accepted(hash)) => {
                tracing::info!(
                    chain_key = %self.store.chain_key,
                    registry_address = %self.store.registry_address,
                    user_id = %job.user_id,
                    attempt_number = job.attempt_number,
                    stage = if creating { "creating_proxy" } else { "registering" },
                    tx_hash = %hash,
                    "Transaction submitted; awaiting confirmation"
                );
                self.store
                    .transition(
                        job,
                        if creating {
                            "proxy_pending"
                        } else {
                            "registration_pending"
                        },
                        None,
                        creating.then_some(hash.as_str()),
                        (!creating).then_some(hash.as_str()),
                        None,
                    )
                    .await?;
            }
            Ok(Broadcast::Rejected(error)) => {
                tracing::error!(
                    chain_key = %self.store.chain_key,
                    user_id = %job.user_id,
                    attempt_number = job.attempt_number,
                    stage = if creating { "creating_proxy" } else { "registering" },
                    error = %format_args!("{error:#}"),
                    "Transaction submission rejected"
                );
                self.fail(job, "chain_transaction_rejected").await?;
            }
            Err(error) => {
                tracing::error!(
                    chain_key = %self.store.chain_key,
                    user_id = %job.user_id,
                    attempt_number = job.attempt_number,
                    stage = if creating { "creating_proxy" } else { "registering" },
                    error = %format_args!("{error:#}"),
                    "Transaction submission outcome unknown"
                );
                self.store
                    .transition(
                        job,
                        "unknown",
                        None,
                        None,
                        None,
                        Some("submission_outcome_unknown"),
                    )
                    .await?
            }
        }
        Ok(())
    }
}
