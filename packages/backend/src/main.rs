use std::sync::Arc;

use anyhow::Result;
use app_backend::{
    api::{self, ApiState},
    chain::CosmosChain,
    config::Config,
    store::Store,
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
    Store::migrate(&pool).await?;
    let store = Store {
        pool,
        chain_key: config.chain_key.clone(),
        registry_address: config.registry_address.clone(),
    };
    let app = api::router(ApiState {
        store: store.clone(),
        api_key_hash: Sha256::digest(config.api_key.as_bytes()).into(),
        hourly_quota: config.registrations_per_hour,
        daily_quota: config.registrations_per_day,
    });
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(address = %config.listen, provisioning_enabled = config.provisioning_enabled, "Backend listening");
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());
    if config.provisioning_enabled {
        let worker = Worker {
            store,
            chain: Arc::new(CosmosChain::connect(&config).await?),
        };
        tokio::select! {
            result = server => result?,
            result = worker.run(&config.database_url) => result?,
        }
    } else {
        server.await?;
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
