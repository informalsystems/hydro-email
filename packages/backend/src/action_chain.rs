use anyhow::{ensure, Result};
use app_contract_api::service_handler::msg::{CustomQueryMsg, EventProcessedResponse};
use async_trait::async_trait;
use layer_climb::prelude::*;
use tokio::sync::OnceCell;

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionOutcome {
    Succeeded,
    Failed,
    NotFound,
}

#[async_trait]
pub trait ActionChain: Send + Sync {
    async fn transaction(&self, hash: &str) -> Result<TransactionOutcome>;
    async fn event_processed(&self, event_id: &str) -> Result<bool>;
}

// This client has no signer or broadcast methods.
pub struct CosmosActionChain {
    client: OnceCell<QueryClient>,
    chain_config: ChainConfig,
    handler: Address,
}

impl CosmosActionChain {
    pub async fn from_config(config: &Config) -> Result<Self> {
        let chains = app_utils::config::load_chain_configs_from_wavs(None::<&str>).await?;
        let chain = chains
            .get_chain(&config.chain_key.parse()?)
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Unknown BACKEND_CHAIN_KEY"))?
            .to_cosmos_config()?;
        let chain_config: ChainConfig = chain.into();
        let handler = chain_config.parse_address(&config.service_handler_address)?;
        Ok(Self {
            client: OnceCell::new(),
            chain_config,
            handler,
        })
    }

    async fn client(&self) -> Result<&QueryClient> {
        // Connection failures are retried by the worker, including after restarts.
        // The worker also bounds initialization with its per-check timeout.
        self.client
            .get_or_try_init(|| async {
                QueryClient::new(self.chain_config.clone(), None)
                    .await
                    .map_err(|_| anyhow::anyhow!("Chain query client unavailable"))
            })
            .await
    }
}

#[async_trait]
impl ActionChain for CosmosActionChain {
    async fn transaction(&self, hash: &str) -> Result<TransactionOutcome> {
        let client = self.client().await?;
        let request = layer_climb::proto::tx::GetTxRequest {
            hash: hash.to_owned(),
        };
        let response = match client.get_connection_mode() {
            ConnectionMode::Grpc => {
                let mut client = layer_climb::proto::tx::service_client::ServiceClient::new(
                    client.clone_grpc_channel()?,
                );
                match client.get_tx(request).await {
                    Ok(response) => response.into_inner(),
                    Err(error) if error.code() == tonic::Code::NotFound => {
                        return Ok(TransactionOutcome::NotFound);
                    }
                    Err(_) => anyhow::bail!("Transaction query unavailable"),
                }
            }
            ConnectionMode::Rpc => {
                match client
                    .rpc_client()?
                    .abci_protobuf_query::<_, layer_climb::proto::tx::GetTxResponse>(
                        "/cosmos.tx.v1beta1.Service/GetTx",
                        request,
                        None,
                    )
                    .await
                {
                    Ok(response) => response,
                    Err(error) if missing_transaction(&error.to_string(), hash) => {
                        return Ok(TransactionOutcome::NotFound);
                    }
                    Err(_) => anyhow::bail!("Transaction query unavailable"),
                }
            }
        };
        let response = response
            .tx_response
            .ok_or_else(|| anyhow::anyhow!("Missing transaction query response"))?;
        ensure!(
            response.height > 0 && response.txhash.eq_ignore_ascii_case(hash),
            "Transaction query did not confirm the requested transaction"
        );
        Ok(if response.code == 0 {
            TransactionOutcome::Succeeded
        } else {
            TransactionOutcome::Failed
        })
    }

    async fn event_processed(&self, event_id: &str) -> Result<bool> {
        let id = const_hex::decode(event_id)?;
        ensure!(id.len() == 20, "Invalid event ID");
        let response: EventProcessedResponse = self
            .client()
            .await?
            .contract_smart(
                &self.handler,
                &CustomQueryMsg::EventProcessed {
                    event_id: id.into(),
                },
            )
            .await
            .map_err(|_| anyhow::anyhow!("Handled-event query unavailable"))?;
        Ok(response.processed)
    }
}

// The RPC client erases ABCI error structure. Require the explicit GetTx
// NotFound error and requested hash; transport failures are never absence.
fn missing_transaction(error: &str, hash: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.starts_with("abci query failed: ")
        && error.contains("code = notfound")
        && error.contains(&hash.to_ascii_lowercase())
        && error.contains("not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_missing_transactions_are_absence() {
        let hash = "AB".repeat(32);
        assert!(missing_transaction(
            &format!("abci query failed: rpc error: code = NotFound desc = tx ({hash}) not found"),
            &hash
        ));
        for error in [
            "HTTP 404 Not Found".to_owned(),
            "connection timeout".to_owned(),
            "abci query failed: rpc error: code = Unavailable desc = not found".to_owned(),
            format!(
                "abci query failed: rpc error: code = NotFound desc = tx ({}) not found",
                "CD".repeat(32)
            ),
        ] {
            assert!(!missing_transaction(&error, &hash));
        }
    }
}
