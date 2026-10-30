use crate::context::CliContext;
use anyhow::{ensure, Result};
use app_contract_api::{service_manager as manager, stake_registry as registry};
use cosmwasm_std::Uint256;
use layer_climb::{prelude::*, proto::proto_into_any};
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::PathBuf};
use wavs_types::contracts::cosmwasm::service_manager::{
    QuorumThreshold, ServiceManagerExecuteMessages as WavsExecute,
    ServiceManagerQueryMessages as WavsQuery,
};

async fn write_request(ctx: &CliContext, request: Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&request)?);
    if ctx.args().output_file.is_some() {
        ctx.args().output().write(request).await?;
    }
    Ok(())
}

pub async fn execute_or_prepare(
    ctx: &CliContext,
    address: &str,
    msg: Value,
    broadcast: bool,
) -> Result<()> {
    execute_request(ctx, address, msg, broadcast, false).await
}

/// Optional sample registration uses the existing dedicated API credential.
/// Normal infrastructure deployment never needs this mnemonic.
pub async fn register_or_prepare(
    ctx: &CliContext,
    address: &str,
    msg: Value,
    broadcast: bool,
) -> Result<()> {
    execute_request(ctx, address, msg, broadcast, true).await
}

pub async fn configure_user_registry(
    ctx: &CliContext,
    address: &str,
    proxy_code_id: u64,
    handler: &str,
    centers: &str,
    owner: &str,
) -> Result<()> {
    use app_contract_api::user_registry::msg::{ConfigResponse, ExecuteMsg, QueryMsg};
    let address = ctx.parse_address(address).await?;
    let owner = ctx.parse_address(owner).await?.to_string();
    let handler = ctx.parse_address(handler).await?.to_string();
    let centers = ctx.parse_address(centers).await?.to_string();
    let query = ctx.query_client().await?;
    let current: ConfigResponse = query.contract_smart(&address, &QueryMsg::Config {}).await?;
    let mut messages = Vec::new();
    if let Some(proxy) = current.proxy {
        ensure!(
            proxy.proxy_code_id == proxy_code_id
                && proxy.service_handler.as_str() == handler
                && proxy.control_center_registry.as_str() == centers,
            "Registry factory configuration mismatch"
        );
    } else {
        messages.push(ExecuteMsg::ConfigureProxy {
            proxy_code_id,
            service_handler: handler.clone(),
            control_center_registry: centers.clone(),
        });
    }
    if current.owner.as_str() != owner {
        messages.push(ExecuteMsg::TransferOwnership {
            owner: owner.clone(),
        });
    }
    let mut record =
        json!({"registry": address.to_string(), "owner": owner, "status": "already_configured"});
    if !messages.is_empty() {
        let client = ctx.signing_client().await?;
        ensure!(
            current.owner.as_str() == client.addr.to_string(),
            "CLI wallet is not registry owner"
        );
        let tx = messages
            .iter()
            .map(|msg| proto_into_any(&client.contract_execute_msg(&address, vec![], msg)?))
            .collect::<Result<Vec<_>>>()?;
        let response = client.tx_builder().broadcast(tx).await?;
        ensure!(
            response.code == 0,
            "Registry configuration transaction failed"
        );
        record["status"] = json!("configured");
        record["tx_hash"] = json!(response.txhash);
    }
    let configured: ConfigResponse = query.contract_smart(&address, &QueryMsg::Config {}).await?;
    let proxy = configured
        .proxy
        .ok_or_else(|| anyhow::anyhow!("Registry factory is not configured"))?;
    ensure!(
        configured.owner.as_str() == owner
            && proxy.proxy_code_id == proxy_code_id
            && proxy.service_handler.as_str() == handler
            && proxy.control_center_registry.as_str() == centers,
        "Registry configuration verification failed"
    );
    write_request(ctx, record).await
}

async fn execute_request(
    ctx: &CliContext,
    address: &str,
    msg: Value,
    broadcast: bool,
    backend_signer: bool,
) -> Result<()> {
    let address = ctx.parse_address(address).await?;
    let mut request = json!({"chain": ctx.chain_key().to_string(), "contract": address.to_string(), "msg": msg, "funds": [], "status": "prepared"});
    if broadcast {
        // Existing operator entries can be verified without another transaction,
        // including after ownership has already moved to the committee.
        if let Ok(registry::ExecuteMsg::SetOperatorDetails {
            operator,
            signing_key,
            weight,
        }) = serde_json::from_value(msg.clone())
        {
            let query = ctx.query_client().await?;
            let current_key: Option<EvmAddr> = query
                .contract_smart(
                    &address,
                    &registry::QueryMsg::GetOperatorSigningKey {
                        operator: operator.clone(),
                    },
                )
                .await?;
            let current_weight: Uint256 = query
                .contract_smart(
                    &address,
                    &registry::QueryMsg::GetOperatorWeight { operator },
                )
                .await?;
            if current_key == Some(signing_key) && current_weight == weight {
                request["status"] = json!("already_configured");
                return write_request(ctx, request).await;
            }
        }
        let client = if backend_signer {
            ctx.backend_signing_client().await?
        } else {
            ctx.signing_client().await?
        };
        let response = client
            .contract_execute(&address, &msg, vec![], None)
            .await?;
        request["tx_hash"] = json!(response.txhash);
        request["status"] = json!("submitted");
    }
    write_request(ctx, request).await
}

pub fn manager_instantiate(code_id: u64, admin: String) -> cosmwasm_std::WasmMsg {
    cosmwasm_std::WasmMsg::Instantiate2 {
        admin: None,
        code_id,
        msg: cosmwasm_std::to_json_binary(&manager::InstantiateMsg { admin }).unwrap(),
        funds: vec![],
        label: "Mirror Service Manager".to_owned(),
        salt: cosmwasm_std::Binary::from(b"service_manager"),
    }
}

fn parse_uint(value: &str) -> Result<Uint256> {
    value
        .parse()
        .map_err(|error: cosmwasm_std::StdError| anyhow::anyhow!(error.to_string()))
}

pub fn configure_messages(
    uri: &str,
    numerator: &str,
    denominator: &str,
) -> Result<Vec<manager::ExecuteMsg>> {
    let numerator = parse_uint(numerator)?;
    let denominator = parse_uint(denominator)?;
    ensure!(!uri.trim().is_empty(), "Service URI must not be empty");
    ensure!(
        !numerator.is_zero() && !denominator.is_zero() && numerator <= denominator,
        "Quorum must satisfy 0 < numerator <= denominator"
    );
    Ok(vec![
        manager::ExecuteMsg::Wavs(WavsExecute::WavsSetServiceUri {
            service_uri: uri.to_owned(),
        }),
        manager::ExecuteMsg::Wavs(WavsExecute::WavsSetQuorumThreshold {
            numerator,
            denominator,
        }),
    ])
}

pub async fn verify_service_uri(ctx: &CliContext, address: &str, uri: &str) -> Result<()> {
    let query = ctx.query_client().await?;
    let address = ctx.parse_address(address).await?;
    let actual: String = query
        .contract_smart(
            &address,
            &manager::QueryMsg::Wavs(WavsQuery::WavsServiceUri {}),
        )
        .await?;
    ensure!(
        actual == uri,
        "On-chain service URI does not match the uploaded service"
    );
    Ok(())
}

async fn verify_quorum(
    ctx: &CliContext,
    address: &str,
    numerator: &str,
    denominator: &str,
) -> Result<()> {
    let query = ctx.query_client().await?;
    let address = ctx.parse_address(address).await?;
    let actual: QuorumThreshold = query
        .contract_smart(
            &address,
            &manager::QueryMsg::Wavs(WavsQuery::WavsQuorumThreshold {}),
        )
        .await?;
    ensure!(
        actual.numerator == parse_uint(numerator)?
            && actual.denominator == parse_uint(denominator)?,
        "On-chain quorum does not match the deployment configuration"
    );
    Ok(())
}

pub async fn configure_middleware(
    ctx: &CliContext,
    address: &str,
    uri: &str,
    numerator: &str,
    denominator: &str,
) -> Result<()> {
    let messages = configure_messages(uri, numerator, denominator)?;
    let query = ctx.query_client().await?;
    let address = ctx.parse_address(address).await?;
    let actual_uri: String = query
        .contract_smart(
            &address,
            &manager::QueryMsg::Wavs(WavsQuery::WavsServiceUri {}),
        )
        .await?;
    let actual: QuorumThreshold = query
        .contract_smart(
            &address,
            &manager::QueryMsg::Wavs(WavsQuery::WavsQuorumThreshold {}),
        )
        .await?;
    let mut record = json!({"service_manager_address": address.to_string(), "uri": uri});
    if actual_uri == uri
        && actual.numerator == parse_uint(numerator)?
        && actual.denominator == parse_uint(denominator)?
    {
        record["status"] = json!("already_configured");
    } else {
        let current_admin: String = query
            .contract_smart(
                &address,
                &manager::QueryMsg::Mirror(manager::MirrorServiceManagerQueryMessages::Admin {}),
            )
            .await?;
        let client = ctx.signing_client().await?;
        ensure!(
            current_admin == client.addr.to_string(),
            "CLI wallet is not the service-manager admin"
        );
        let tx = messages
            .iter()
            .map(|msg| proto_into_any(&client.contract_execute_msg(&address, vec![], msg)?))
            .collect::<Result<Vec<_>>>()?;
        let response = client.tx_builder().broadcast(tx).await?;
        ensure!(
            response.code == 0,
            "Manager configuration transaction failed"
        );
        record["tx_hash"] = json!(response.txhash);
        verify_service_uri(ctx, &address.to_string(), uri).await?;
        verify_quorum(ctx, &address.to_string(), numerator, denominator).await?;
        record["status"] = json!("configured");
    }
    write_request(ctx, record).await
}

/// Require every expected public operator request to match the actual chain state
/// before relinquishing the deployment wallet's operator-management authority.
async fn verify_operators(ctx: &CliContext, registry: &Address, files: &[PathBuf]) -> Result<()> {
    ensure!(
        !files.is_empty(),
        "Expected operator requests are required before handoff"
    );
    let query = ctx.query_client().await?;
    let mut operators = BTreeSet::new();
    let mut keys = BTreeSet::new();
    let mut planned_weight = Uint256::zero();
    for file in files {
        let request: Value = serde_json::from_slice(&tokio::fs::read(file).await?)?;
        ensure!(
            request["chain"] == ctx.chain_key().to_string()
                && request["contract"] == registry.to_string(),
            "Operator request belongs to another chain or registry: {}",
            file.display()
        );
        let registry::ExecuteMsg::SetOperatorDetails {
            operator,
            signing_key,
            weight,
        } = serde_json::from_value(request["msg"].clone())?
        else {
            anyhow::bail!(
                "Expected an operator key/weight request: {}",
                file.display()
            )
        };
        ensure!(
            operators.insert(operator.to_string()) && keys.insert(signing_key.to_string()),
            "Duplicate operator or signing key in deployment requests"
        );
        ensure!(
            !weight.is_zero(),
            "Initial operator voting weights must be positive"
        );
        let actual_key: Option<EvmAddr> = query
            .contract_smart(
                registry,
                &registry::QueryMsg::GetOperatorSigningKey {
                    operator: operator.clone(),
                },
            )
            .await?;
        let actual_weight: Uint256 = query
            .contract_smart(
                registry,
                &registry::QueryMsg::GetOperatorWeight {
                    operator: operator.clone(),
                },
            )
            .await?;
        let reverse: Option<EvmAddr> = query
            .contract_smart(
                registry,
                &registry::QueryMsg::GetLatestOperatorForSigningKey {
                    signing_key: signing_key.clone(),
                },
            )
            .await?;
        ensure!(
            actual_key == Some(signing_key) && actual_weight == weight && reverse == Some(operator),
            "On-chain operator key/weight does not match {}",
            file.display()
        );
        planned_weight = planned_weight
            .checked_add(weight)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    }
    let total: Uint256 = query
        .contract_smart(registry, &registry::QueryMsg::GetTotalWeight {})
        .await?;
    ensure!(
        total == planned_weight && !total.is_zero(),
        "On-chain total weight does not match the full deployment operator set"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn finalize_middleware(
    ctx: &CliContext,
    address: &str,
    registry_address: &str,
    uri: &str,
    committee: &str,
    numerator: &str,
    denominator: &str,
    operator_files: &[PathBuf],
) -> Result<()> {
    configure_messages(uri, numerator, denominator)?;
    verify_service_uri(ctx, address, uri).await?;
    verify_quorum(ctx, address, numerator, denominator).await?;
    let query = ctx.query_client().await?;
    let manager = ctx.parse_address(address).await?;
    let registry = ctx.parse_address(registry_address).await?;
    let committee = ctx.parse_address(committee).await?.to_string();
    let manager_registry: String = query
        .contract_smart(
            &manager,
            &manager::QueryMsg::Mirror(
                manager::MirrorServiceManagerQueryMessages::StakeRegistry {},
            ),
        )
        .await?;
    let registry_manager: String = query
        .contract_smart(&registry, &registry::QueryMsg::GetServiceManager {})
        .await?;
    ensure!(
        manager_registry == registry.to_string() && registry_manager == manager.to_string(),
        "Middleware contracts are not linked to each other"
    );
    verify_operators(ctx, &registry, operator_files).await?;
    let current_admin: String = query
        .contract_smart(
            &manager,
            &manager::QueryMsg::Mirror(manager::MirrorServiceManagerQueryMessages::Admin {}),
        )
        .await?;
    let current_owner: String = query
        .contract_smart(&registry, &registry::QueryMsg::Owner {})
        .await?;
    let mut record = json!({"service_manager_address": address, "registry_address": registry_address, "committee_address": committee, "uri": uri});
    if current_admin == committee && current_owner == committee {
        record["status"] = json!("already_finalized");
    } else {
        let client = ctx.signing_client().await?;
        ensure!(
            (current_admin == committee || current_admin == client.addr.to_string())
                && (current_owner == committee || current_owner == client.addr.to_string()),
            "CLI wallet does not control the remaining middleware authority"
        );
        let mut tx = Vec::new();
        if current_admin != committee {
            tx.push(proto_into_any(&client.contract_execute_msg(
                &manager,
                vec![],
                &manager::ExecuteMsg::TransferAdmin {
                    new_admin: committee.clone(),
                },
            )?)?);
        }
        if current_owner != committee {
            tx.push(proto_into_any(&client.contract_execute_msg(
                &registry,
                vec![],
                &registry::ExecuteMsg::TransferOwnership {
                    new_owner: committee.clone(),
                },
            )?)?);
        }
        let response = client.tx_builder().broadcast(tx).await?;
        ensure!(response.code == 0, "Middleware ownership handoff failed");
        println!(
            "Middleware ownership handoff transaction: {}",
            response.txhash
        );
        record["tx_hash"] = json!(response.txhash);
        let actual_admin: String = query
            .contract_smart(
                &manager,
                &manager::QueryMsg::Mirror(manager::MirrorServiceManagerQueryMessages::Admin {}),
            )
            .await?;
        let actual_owner: String = query
            .contract_smart(&registry, &registry::QueryMsg::Owner {})
            .await?;
        ensure!(
            actual_admin == committee && actual_owner == committee,
            "Middleware handoff verification failed"
        );
        record["status"] = json!("finalized");
    }
    write_request(ctx, record).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::{from_json, WasmMsg};
    #[test]
    fn configuration_has_no_early_transfer_and_rejects_invalid_quorum() {
        let messages = configure_messages("ipfs://service/service.json", "2", "3").unwrap();
        let wire: Vec<Value> = messages
            .iter()
            .map(|msg| serde_json::to_value(msg).unwrap())
            .collect();
        assert_eq!(
            wire,
            vec![
                json!({"wavs_set_service_uri": {"service_uri": "ipfs://service/service.json"}}),
                json!({"wavs_set_quorum_threshold": {"numerator": "2", "denominator": "3"}}),
            ]
        );
        assert!(configure_messages("ipfs://service/service.json", "0", "3").is_err());
        assert!(configure_messages("ipfs://service/service.json", "4", "3").is_err());
        assert!(configure_messages("", "2", "3").is_err());
    }
    #[test]
    fn manager_has_temporary_admin_and_no_migration_admin() {
        let WasmMsg::Instantiate2 {
            admin,
            code_id,
            msg,
            ..
        } = manager_instantiate(42, "deployer".to_owned())
        else {
            panic!("expected Instantiate2")
        };
        assert_eq!(admin, None);
        assert_eq!(code_id, 42);
        assert_eq!(
            from_json::<Value>(msg).unwrap(),
            json!({"admin": "deployer"})
        );
    }
}
