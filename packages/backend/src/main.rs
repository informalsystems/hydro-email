use std::sync::Arc;

use anyhow::Result;
use app_backend::{
    action_store::ActionStore,
    api::{self, ApiState},
    chain::CosmosChain,
    config::Config,
    mailbox_worker::{ImapMailbox, MailboxWorker},
    registration_store::RegistrationStore,
    worker::Worker,
};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let config = Config::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&config.database_url)
        .await?;
    RegistrationStore::migrate(&pool).await?;
    let store = RegistrationStore {
        pool,
        chain_key: config.chain_key.clone(),
        registry_address: config.registry_address.clone(),
    };
    let actions = ActionStore {
        pool: store.pool.clone(),
        chain_key: config.chain_key.clone(),
        service_handler_address: config.service_handler_address.clone(),
    };
    let mailbox_worker = MailboxWorker {
        store: actions.clone(),
        mailbox: Arc::new(ImapMailbox::from_env()?),
    };
    let app = api::router(ApiState {
        store: store.clone(),
        actions,
        api_key_hash: Sha256::digest(config.api_key.as_bytes()).into(),
        hourly_quota: config.registrations_per_hour,
        daily_quota: config.registrations_per_day,
    });
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(address = %config.listen, provisioning_enabled = config.provisioning_enabled, "Backend listening");
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());
    let registration_worker = if config.provisioning_enabled {
        Some(Worker {
            store,
            chain: Arc::new(CosmosChain::connect(&config).await?),
        })
    } else {
        None
    };
    tokio::select! {
        result = server => result?,
        result = async {
            match &registration_worker {
                Some(worker) => worker.run(&config.database_url).await,
                None => std::future::pending().await,
            }
        } => result?,
        result = mailbox_worker.run() => result?,
    }
    Ok(())
}

async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = interrupt => {}, _ = terminate => {} }
}
