use std::{collections::BTreeSet, time::Duration};

use anyhow::{ensure, Result};
use app_contract_api::user_registry::msg::{ExecuteMsg, ProxyAddressResponse, QueryMsg, UserId};
use async_trait::async_trait;
use layer_climb::{prelude::*, proto::proto_into_any};

use crate::config::Config;

pub enum Confirmation {
    Pending,
    Succeeded,
    Failed,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProxyState {
    Missing,
    Ready,
    Invalid,
}

pub enum Broadcast {
    Accepted(String),
    Rejected(anyhow::Error),
}

#[async_trait]
pub trait RegistrationChain: Send + Sync {
    async fn lookup(&self, user_id: &str) -> Result<Option<String>>;
    async fn proxy_address(&self, user_id: &str) -> Result<String>;
    async fn proxy_state(&self, address: &str) -> Result<ProxyState>;
    async fn create_proxy(&self, user_id: &str) -> Result<Broadcast>;
    async fn register(&self, user_id: &str, proxy: &str) -> Result<Broadcast>;
    async fn confirm(&self, hash: &str) -> Result<Confirmation>;
}

pub struct CosmosChain {
    client: SigningClient,
    registry: Address,
    code_id: u64,
    service_handler: String,
    control_centers: Vec<String>,
}

impl CosmosChain {
    pub async fn connect(config: &Config) -> Result<Self> {
        let chains = app_utils::config::load_chain_configs_from_wavs(None::<&str>).await?;
        let chain = chains
            .get_chain(&config.chain_key.parse()?)
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Unknown BACKEND_CHAIN_KEY"))?
            .to_cosmos_config()?;
        let signer = KeySigner::new_mnemonic_str(&config.signing_mnemonic, None)?;
        let client = SigningClient::new(chain.into(), signer, None).await?;
        let registry = client
            .querier
            .chain_config
            .parse_address(&config.registry_address)?;
        client.querier.contract_info(&registry).await?;
        client
            .querier
            .contract_code_info(config.proxy_code_id)
            .await?;
        for address in
            std::iter::once(&config.service_handler_address).chain(&config.control_centers)
        {
            let address = client.querier.chain_config.parse_address(address)?;
            client.querier.contract_info(&address).await?;
        }
        // The current registry has no admin query. Simulating a registration
        // checks signer authorization before we pay to create any proxy. Simulation
        // does not broadcast or commit the temporary registry write.
        let probe = UserId::new_email_address(&format!(
            "backend-preflight-{}@invalid.example",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let probe_msg = ExecuteMsg::RegisterUser {
            user_id: probe,
            proxy_address: config.service_handler_address.clone(),
        };
        let message = client.contract_execute_msg(&registry, vec![], &probe_msg)?;
        let builder = client.tx_builder();
        let signer_info = client
            .signer
            .signer_info(
                builder.current_sequence().await?,
                layer_climb::proto::tx::signing::SignMode::Unspecified,
            )
            .await?;
        let mut body = layer_climb::proto::tx::TxBody {
            messages: vec![proto_into_any(&message)?],
            ..Default::default()
        };
        builder
            .simulate_gas(signer_info, client.account_number, &mut body)
            .await?;
        Ok(Self {
            client,
            registry,
            code_id: config.proxy_code_id,
            service_handler: config.service_handler_address.clone(),
            control_centers: config.control_centers.clone(),
        })
    }

    async fn broadcast(&self, message: layer_climb::proto::Any) -> Result<Broadcast> {
        // No automatic broadcast retry. Obtain a CheckTx hash immediately, then
        // let the worker query inclusion independently of the submission request.
        let mut builder = self.client.tx_builder();
        // Leave gas unset so the chain client estimates it through simulation.
        builder.set_broadcast_poll(false);
        let response = match builder.broadcast_raw(vec![message]).await {
            Ok(response) => response,
            // The pinned Climb SDK discards the structured CheckTx rejection and
            // returns this error. All other errors remain conservatively unknown.
            Err(error) if error.to_string().starts_with("tx failed with code: ") => {
                return Ok(Broadcast::Rejected(error));
            }
            Err(error) => return Err(error),
        };
        if response.code() != 0 {
            return Ok(Broadcast::Rejected(anyhow::anyhow!(
                "tx failed with code: {}, codespace: {}, raw_log: {}",
                response.code(),
                response.codespace(),
                response.raw_log()
            )));
        }
        Ok(Broadcast::Accepted(response.tx_hash()))
    }
}

#[async_trait]
impl RegistrationChain for CosmosChain {
    async fn lookup(&self, user_id: &str) -> Result<Option<String>> {
        let query = QueryMsg::ProxyAddress {
            user_id: UserId::new_raw(user_id.to_owned()),
        };
        match self
            .client
            .querier
            .contract_smart::<ProxyAddressResponse, _>(&self.registry, &query)
            .await
        {
            Ok(response) => Ok(Some(response.address.to_string())),
            // Registry currently uses a contract error rather than an optional response.
            // Network/RPC errors must never be interpreted as an absent registration.
            Err(error) if error.to_string().contains("no proxy address for user:") => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn proxy_address(&self, user_id: &str) -> Result<String> {
        Ok(self
            .client
            .querier
            .contract_predict_address(self.code_id, &self.client.addr, &proxy_salt(user_id)?)
            .await?
            .to_string())
    }

    async fn proxy_state(&self, proxy: &str) -> Result<ProxyState> {
        let address = self.client.querier.chain_config.parse_address(proxy)?;
        let info = match self.client.querier.contract_info(&address).await {
            Ok(info) => info.contract_info,
            Err(error) if missing_contract(&error.to_string(), proxy) => {
                return Ok(ProxyState::Missing)
            }
            Err(error) => return Err(error),
        };
        let Some(info) = info else {
            return Ok(ProxyState::Missing);
        };
        if info.code_id != self.code_id
            || info.creator != self.client.addr.to_string()
            || !info.admin.is_empty()
        {
            return Ok(ProxyState::Invalid);
        }
        let response: hydro_proxy::msg::ConfigResponse = self
            .client
            .querier
            .contract_smart(&address, &hydro_proxy::msg::QueryMsg::Config {})
            .await?;
        let admins: Vec<_> = response
            .config
            .admins
            .iter()
            .map(ToString::to_string)
            .collect();
        let centers: BTreeSet<_> = response
            .config
            .control_centers
            .iter()
            .map(ToString::to_string)
            .collect();
        let expected_centers: BTreeSet<_> = self.control_centers.iter().cloned().collect();
        Ok(
            if admins == vec![self.service_handler.clone()] && centers == expected_centers {
                ProxyState::Ready
            } else {
                ProxyState::Invalid
            },
        )
    }

    async fn create_proxy(&self, user_id: &str) -> Result<Broadcast> {
        let msg = hydro_proxy::msg::InstantiateMsg {
            admins: vec![self.service_handler.clone()],
            control_centers: self.control_centers.clone(),
        };
        let message = self.client.contract_instantiate2_msg(
            None,
            self.code_id,
            "Hydro Email Proxy",
            vec![],
            proxy_salt(user_id)?,
            false,
            &msg,
        )?;
        self.broadcast(proto_into_any(&message)?).await
    }

    async fn register(&self, user_id: &str, proxy: &str) -> Result<Broadcast> {
        ensure!(
            self.proxy_state(proxy).await? == ProxyState::Ready,
            "Unexpected proxy configuration"
        );
        let msg = ExecuteMsg::RegisterUser {
            user_id: UserId::new_raw(user_id.to_owned()),
            proxy_address: proxy.to_owned(),
        };
        let message = self
            .client
            .contract_execute_msg(&self.registry, vec![], &msg)?;
        self.broadcast(proto_into_any(&message)?).await
    }

    async fn confirm(&self, hash: &str) -> Result<Confirmation> {
        let result = self
            .client
            .querier
            .poll_until_tx_ready(
                hash.to_owned(),
                Duration::from_millis(100),
                Duration::from_millis(100),
            )
            .await;
        let response = match result {
            Ok(response) => response.tx_response,
            Err(_) => return Ok(Confirmation::Pending),
        };
        if response.code != 0 {
            return Ok(Confirmation::Failed);
        }
        Ok(Confirmation::Succeeded)
    }
}

fn proxy_salt(user_id: &str) -> Result<Vec<u8>> {
    let salt = const_hex::decode(user_id)?;
    ensure!(salt.len() == 32, "Expected a SHA-256 email ID");
    Ok(salt)
}

// Recognize contract absence only when the chain names the queried address;
// an RPC outage or an HTTP 404 is not evidence that a proxy does not exist.
fn missing_contract(error: &str, address: &str) -> bool {
    let error = error.to_lowercase();
    error.contains(address)
        && (error.contains("no such contract")
            || error.contains(&format!("contract: {address}: not found")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::Api;
    use cw_multi_test::{App, ContractWrapper, Executor};

    #[test]
    fn email_salt_predicts_one_proxy_and_duplicate_creation_fails() {
        let mut app = App::default();
        let creator = app.api().addr_make("registration-wallet");
        let handler = app.api().addr_make("handler");
        let center = app.api().addr_make("center");
        let code = ContractWrapper::new(
            hydro_proxy::contract::execute,
            hydro_proxy::contract::instantiate,
            hydro_proxy::contract::query,
        );
        let code_id = app.store_code(Box::new(code));
        let msg = hydro_proxy::msg::InstantiateMsg {
            admins: vec![handler.to_string()],
            control_centers: vec![center.to_string()],
        };
        let id = UserId::new_email_address("alice@example.com");
        let salt = proxy_salt(id.as_str()).unwrap();
        assert_eq!(salt.len(), 32);
        let checksum = app.wrap().query_wasm_code_info(code_id).unwrap().checksum;
        let creator_bytes = app.api().addr_canonicalize(creator.as_str()).unwrap();
        let expected =
            cosmwasm_std::instantiate2_address(checksum.as_slice(), &creator_bytes, &salt).unwrap();
        let expected = app.api().addr_humanize(&expected).unwrap();
        let proxy = app
            .instantiate2_contract(
                code_id,
                creator.clone(),
                &msg,
                &[],
                "Proxy",
                None,
                salt.clone(),
            )
            .unwrap();
        assert_eq!(proxy, expected);
        assert!(app
            .instantiate2_contract(code_id, creator.clone(), &msg, &[], "Proxy", None, salt)
            .is_err());
        let other_salt = proxy_salt(UserId::new_email_address("bob@example.com").as_str()).unwrap();
        let other = app
            .instantiate2_contract(code_id, creator, &msg, &[], "Proxy", None, other_salt)
            .unwrap();
        assert_ne!(proxy, other);
    }

    #[test]
    fn rpc_failure_is_not_contract_absence() {
        assert!(missing_contract(
            "address cosmos1proxy: no such contract",
            "cosmos1proxy"
        ));
        assert!(missing_contract(
            "contract: cosmos1proxy: not found",
            "cosmos1proxy"
        ));
        assert!(!missing_contract("HTTP 404 Not Found", "cosmos1proxy"));
        assert!(!missing_contract("connection timeout", "cosmos1proxy"));
        assert!(!missing_contract(
            "address cosmos1other: no such contract",
            "cosmos1proxy"
        ));
        assert!(proxy_salt("invalid").is_err());
    }
}
