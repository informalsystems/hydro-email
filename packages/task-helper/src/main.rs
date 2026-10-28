mod admin;
mod command;
mod config;
mod context;
mod ipfs;
mod output;

use std::process::exit;

use app_contract_api::user_registry::msg::UserId;
use app_utils::{faucet, tracing::tracing_init};
use cosmwasm_std::Uint256;
use layer_climb::prelude::EvmAddr;
use reqwest::Url;
use serde::{de::DeserializeOwned, Deserialize};
use wavs_types::{
    ComponentSource, GetSignerRequest, Service, ServiceManager, SignatureKind, SignerResponse,
    Submit, Trigger, Workflow,
};

use crate::{
    command::{AuthKind, CliCommand, ContractKind},
    config::path_deployments,
    context::CliContext,
    ipfs::IpfsFile,
    output::{
        OutputComponentUpload, OutputContractInstantiate, OutputContractUpload, OutputServiceUpload,
    },
};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // Install rustls crypto provider before any TLS operations
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    tracing_init();

    let ctx = CliContext::new().await;

    match ctx.command.clone() {
        CliCommand::InstantiateMiddleware {
            manager_code_id,
            registry_code_id,
            committee_address,
            args,
        } => {
            let client = ctx.signing_client().await.unwrap();

            let owner = ctx
                .parse_address(&committee_address)
                .await
                .unwrap()
                .to_string();
            let msg = app_contract_api::stake_registry::InstantiateMsg {
                owner: client.addr.to_string(),
                service_manager_instantiate: admin::manager_instantiate(
                    manager_code_id,
                    client.addr.to_string(),
                ),
            };
            let (registry, response) = client
                .contract_instantiate(
                    None,
                    registry_code_id,
                    "Hydro Email Stake Registry",
                    &msg,
                    vec![],
                    None,
                )
                .await
                .unwrap();
            let manager: String = client
                .querier
                .contract_smart(
                    &registry,
                    &app_contract_api::stake_registry::QueryMsg::GetServiceManager {},
                )
                .await
                .unwrap();
            println!("Stake registry: {registry}\nService manager: {manager}");
            args.output()
                .write(serde_json::json!({
                    "contract_kind": "mirror_stake",
                    "registry_address": registry.to_string(),
                    "committee_address": owner,
                    "bootstrap_admin": client.addr.to_string(),
                    "service_manager_address": manager,
                    "tx_hash": response.txhash,
                }))
                .await
                .unwrap();
        }
        CliCommand::ConfigureMiddleware {
            address,
            uri,
            numerator,
            denominator,
            ..
        } => {
            admin::configure_middleware(&ctx, &address, &uri, &numerator, &denominator)
                .await
                .unwrap();
        }
        CliCommand::FinalizeMiddleware {
            address,
            registry_address,
            uri,
            committee_address,
            numerator,
            denominator,
            operator_files,
            ..
        } => {
            admin::finalize_middleware(
                &ctx,
                &address,
                &registry_address,
                &uri,
                &committee_address,
                &numerator,
                &denominator,
                &operator_files,
            )
            .await
            .unwrap();
        }
        CliCommand::SetServiceUri {
            address,
            uri,
            broadcast,
            ..
        } => {
            admin::execute_or_prepare(
                &ctx,
                &address,
                serde_json::json!({"wavs_set_service_uri": {"service_uri": uri}}),
                broadcast,
            )
            .await
            .unwrap();
        }
        CliCommand::SetQuorum {
            address,
            numerator,
            denominator,
            broadcast,
            ..
        } => {
            let numerator: Uint256 = numerator.parse().expect("Invalid numerator");
            let denominator: Uint256 = denominator.parse().expect("Invalid denominator");
            assert!(
                !numerator.is_zero() && !denominator.is_zero() && numerator <= denominator,
                "Quorum must satisfy 0 < numerator <= denominator"
            );
            admin::execute_or_prepare(&ctx, &address,
                serde_json::json!({"wavs_set_quorum_threshold": {"numerator": numerator, "denominator": denominator}}), broadcast,
            ).await.unwrap();
        }
        CliCommand::VerifyServiceUri { address, uri, .. } => {
            admin::verify_service_uri(&ctx, &address, &uri)
                .await
                .unwrap();
            println!("Service URI confirmed on-chain");
        }
        CliCommand::OperatorSetSigningKey {
            service_manager_address,
            evm_operator_address,
            stake_registry_address,
            weight,
            broadcast,
            wavs_url,
            args: _,
        } => {
            let service_manager_address =
                ctx.parse_address(&service_manager_address).await.unwrap();
            let stake_registry_address = ctx.parse_address(&stake_registry_address).await.unwrap();

            let service_manager = ServiceManager::Cosmos {
                chain: ctx.chain_key(),
                address: service_manager_address.clone().try_into().unwrap(),
            };

            let body = serde_json::to_string(&GetSignerRequest { service_manager }).unwrap();

            let url = wavs_url.join("services/signer").unwrap();
            let SignerResponse::Secp256k1 {
                evm_address: evm_signing_key_address,
                hd_index: _,
            } = reqwest::Client::new()
                .post(url.clone())
                .header("Content-Type", "application/json")
                .body(body)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();

            // Parse EVM addresses
            let evm_operator_address: EvmAddr = evm_operator_address.parse().unwrap_or_else(|_| {
                panic!("Invalid operator EVM address '{}'", evm_operator_address)
            });

            let evm_signing_key_address: EvmAddr =
                evm_signing_key_address.parse().unwrap_or_else(|_| {
                    panic!(
                        "Invalid signing key EVM address '{}'",
                        evm_signing_key_address
                    )
                });

            // Parse weight as Uint256
            let weight_uint: Uint256 = weight
                .parse()
                .unwrap_or_else(|e| panic!("Invalid weight '{}': {}", weight, e));

            let set_operator_details_msg =
                app_contract_api::stake_registry::ExecuteMsg::SetOperatorDetails {
                    operator: evm_operator_address,
                    signing_key: evm_signing_key_address,
                    weight: weight_uint,
                };

            admin::execute_or_prepare(
                &ctx,
                &stake_registry_address.to_string(),
                serde_json::to_value(set_operator_details_msg).unwrap(),
                broadcast,
            )
            .await
            .unwrap();
        }
        CliCommand::AssertAccountExists { addr, args: _ } => {
            let client = ctx.query_client().await.unwrap();
            let addr = match addr {
                Some(addr) => ctx.parse_address(&addr).await.unwrap(),
                None => ctx.wallet_addr().await.unwrap(),
            };
            let balance = client
                .balance(addr.clone(), None)
                .await
                .unwrap()
                .unwrap_or_default();

            if balance == 0 {
                eprintln!(
                    "{} has zero balance. Please fund the wallet before proceeding.",
                    addr
                );
                exit(1);
            }
        }
        CliCommand::UploadContract { kind, args } => {
            let client = ctx.signing_client().await.unwrap();

            let (code_id, tx_resp) = client
                .contract_upload_file(kind.wasm_bytes().await, None)
                .await
                .unwrap();

            println!("Uploaded {kind} contract with code ID: {code_id}");

            args.output()
                .write(OutputContractUpload {
                    kind,
                    code_id,
                    tx_hash: tx_resp.txhash,
                })
                .await
                .unwrap();
        }
        CliCommand::InstantiateServiceHandler {
            auth_address,
            auth_kind,
            args,
            code_id,
            user_registry_address,
        } => {
            let client = ctx.signing_client().await.unwrap();

            let auth = match auth_kind {
                AuthKind::ServiceManager => {
                    app_contract_api::service_handler::msg::Auth::ServiceManager(match auth_address
                    {
                        Some(addr) => ctx.parse_address(&addr).await.unwrap().to_string(),
                        None => {
                            panic!("Service manager auth requires an address to be provided")
                        }
                    })
                }
                AuthKind::User => {
                    app_contract_api::service_handler::msg::Auth::Admin(match auth_address {
                        Some(addr) => ctx.parse_address(&addr).await.unwrap().to_string(),
                        None => ctx.wallet_addr().await.unwrap().to_string(),
                    })
                }
            };

            let instantiate_msg = app_contract_api::service_handler::msg::InstantiateMsg {
                auth,
                user_registry: user_registry_address,
            };

            let (contract_addr, tx_resp) = client
                .contract_instantiate(
                    None,
                    code_id,
                    "Service Handler",
                    &instantiate_msg,
                    vec![],
                    None,
                )
                .await
                .unwrap();

            println!("Instantiated Service Handler contract at address: {contract_addr}");

            args.output()
                .write(OutputContractInstantiate {
                    kind: ContractKind::ServiceHandler,
                    address: contract_addr.to_string(),
                    tx_hash: tx_resp.txhash,
                })
                .await
                .unwrap();
        }
        CliCommand::InstantiateUserRegistry {
            args,
            code_id,
            admin,
        } => {
            let client = ctx.signing_client().await.unwrap();

            let instantiate_msg = app_contract_api::user_registry::msg::InstantiateMsg {
                admins: vec![ctx.parse_address(&admin).await.unwrap().to_string()],
            };

            let (contract_addr, tx_resp) = client
                .contract_instantiate(
                    None,
                    code_id,
                    "User Registry",
                    &instantiate_msg,
                    vec![],
                    None,
                )
                .await
                .unwrap();

            println!("Instantiated User Registry contract at address: {contract_addr}");

            args.output()
                .write(OutputContractInstantiate {
                    kind: ContractKind::UserRegistry,
                    address: contract_addr.to_string(),
                    tx_hash: tx_resp.txhash,
                })
                .await
                .unwrap();
        }

        CliCommand::InstantiateLocalControlCenter { args, code_id } => {
            assert_eq!(
                ctx.chain_key().to_string(),
                "dev:local",
                "This empty-vault control center is only for local development"
            );
            let client = ctx.signing_client().await.unwrap();
            // Hydro uses CosmWasm 2.x; the helper uses 3.x. Use its JSON wire format.
            let instantiate_msg = serde_json::json!({
                "deposit_cap": "0",
                "whitelist": [ctx.wallet_addr().await.unwrap().to_string()],
                "subvaults": [],
                "fee_config": null
            });
            let (contract_addr, tx_resp) = client
                .contract_instantiate(
                    None,
                    code_id,
                    "Local Hydro Control Center",
                    &instantiate_msg,
                    vec![],
                    None,
                )
                .await
                .unwrap();
            println!("Instantiated local Hydro control center at address: {contract_addr}");
            args.output()
                .write(OutputContractInstantiate {
                    kind: ContractKind::LocalControlCenter,
                    address: contract_addr.to_string(),
                    tx_hash: tx_resp.txhash,
                })
                .await
                .unwrap();
        }
        CliCommand::InstantiateControlCenterRegistry {
            args,
            code_id,
            admin,
            control_centers,
        } => {
            let client = ctx.signing_client().await.unwrap();
            let admin = match admin {
                Some(admin) => admin,
                None => {
                    assert_eq!(
                        ctx.chain_key().to_string(),
                        "dev:local",
                        "--admin is required on remote chains"
                    );
                    ctx.wallet_addr().await.unwrap().to_string()
                }
            };
            let instantiate_msg = hydro_control_center_registry::msg::InstantiateMsg {
                admin,
                control_centers,
            };
            let (contract_addr, tx_resp) = client
                .contract_instantiate(
                    None,
                    code_id,
                    "Hydro Control Center Registry",
                    &instantiate_msg,
                    vec![],
                    None,
                )
                .await
                .unwrap();
            println!("Instantiated Hydro control-center registry at address: {contract_addr}");
            args.output()
                .write(OutputContractInstantiate {
                    kind: ContractKind::ControlCenterRegistry,
                    address: contract_addr.to_string(),
                    tx_hash: tx_resp.txhash,
                })
                .await
                .unwrap();
        }
        CliCommand::InstantiateProxy {
            admins,
            control_center_registry,
            args,
            code_id,
        } => {
            let client = ctx.signing_client().await.unwrap();

            let instantiate_msg = hydro_proxy::msg::InstantiateMsg {
                admins,
                control_center_registry,
            };

            let (contract_addr, tx_resp) = client
                .contract_instantiate(None, code_id, "Proxy", &instantiate_msg, vec![], None)
                .await
                .unwrap();

            println!("Instantiated Proxy contract at address: {contract_addr}");

            args.output()
                .write(OutputContractInstantiate {
                    kind: ContractKind::Proxy,
                    address: contract_addr.to_string(),
                    tx_hash: tx_resp.txhash,
                })
                .await
                .unwrap();
        }
        CliCommand::FaucetTap {
            addr,
            amount,
            denom,
            args: _,
        } => {
            let client = ctx.query_client().await.unwrap();
            let addr = match addr {
                Some(addr) => ctx.parse_address(&addr).await.unwrap(),
                None => ctx.wallet_addr().await.unwrap(),
            };
            let balance_before = client
                .balance(addr.clone(), None)
                .await
                .unwrap()
                .unwrap_or_default();
            faucet::tap(&ctx.chain_key(), &addr, amount, denom.as_deref())
                .await
                .unwrap();
            let balance_after = client
                .balance(addr.clone(), None)
                .await
                .unwrap()
                .unwrap_or_default();

            println!(
                "Tapped faucet for {addr} - balance before: {balance_before} balance after: {balance_after}"
            );
        }
        CliCommand::UploadComponent {
            kind,
            component,
            args,
            ipfs_kind,
            ipfs_api_url,
            ipfs_gateway_url,
        } => {
            let bytes = kind.wasm_bytes(&component).await;

            let digest = wavs_types::ComponentDigest::hash(&bytes);

            let ipfs_api_url = strip_trailing_slash(&ipfs_api_url);
            let ipfs_gateway_url = strip_trailing_slash(&ipfs_gateway_url);

            let resp = IpfsFile::upload(
                ipfs_kind,
                bytes,
                &format!("{kind}-{component}.wasm"),
                &ipfs_gateway_url,
                &ipfs_api_url,
                true,
            )
            .await
            .unwrap();

            let IpfsFile {
                cid,
                uri,
                gateway_url,
            } = resp;

            args.output()
                .write(OutputComponentUpload {
                    kind,
                    component,
                    digest,
                    cid,
                    uri,
                    gateway_url,
                })
                .await
                .unwrap();
        }
        CliCommand::UploadService {
            args,
            ipfs_kind,
            ipfs_api_url,
            ipfs_gateway_url,
            contract_service_handler_instantiation_file,
            component_operator_email_reader_cid_file,
            component_aggregator_submitter_cid_file,
            trigger_cron_schedule,
            middleware_instantiation_file,
            activate,
        } => {
            let output_directory = path_deployments();

            let contract_service_handler_instantiation_file =
                output_directory.join(contract_service_handler_instantiation_file);
            let component_operator_email_reader_cid_file =
                output_directory.join(component_operator_email_reader_cid_file);
            let component_aggregator_submitter_cid_file =
                output_directory.join(component_aggregator_submitter_cid_file);
            let middleware_instantiation_file =
                output_directory.join(middleware_instantiation_file);

            let ipfs_api_url = strip_trailing_slash(&ipfs_api_url);
            let ipfs_gateway_url = strip_trailing_slash(&ipfs_gateway_url);

            async fn read_and_decode<T: DeserializeOwned>(path: std::path::PathBuf) -> T {
                match tokio::fs::read_to_string(&path).await {
                    Err(e) => {
                        panic!("Failed to read file {}: {}", path.display(), e);
                    }
                    Ok(content) => match serde_json::from_str(&content) {
                        Err(e) => {
                            panic!("Failed to decode JSON from file {}: {}", path.display(), e);
                        }
                        Ok(data) => data,
                    },
                }
            }

            let contract_service_handler: OutputContractInstantiate =
                read_and_decode(contract_service_handler_instantiation_file).await;

            let component_operator_email_reader: OutputComponentUpload =
                read_and_decode(component_operator_email_reader_cid_file).await;

            let component_aggregator_submitter: OutputComponentUpload =
                read_and_decode(component_aggregator_submitter_cid_file).await;

            #[derive(Debug, Deserialize)]
            struct MiddlewareInstantiation {
                #[serde(rename = "registry_address")]
                pub _registry_address: String,
                pub service_manager_address: String,
            }

            let middleware_instantiation: MiddlewareInstantiation =
                read_and_decode(middleware_instantiation_file).await;

            let trigger = Trigger::Cron {
                schedule: trigger_cron_schedule,
                start_time: None,
                end_time: None,
            };

            let operator_email_reader_component = wavs_types::Component {
                source: ComponentSource::Download {
                    //uri: component_operator.uri.parse().unwrap(),
                    uri: component_operator_email_reader.uri.parse().unwrap(),
                    digest: component_operator_email_reader.digest,
                },
                permissions: wavs_types::Permissions {
                    allowed_http_hosts: wavs_types::AllowedHostPermission::All,
                    file_system: false,
                    raw_sockets: true,
                    dns_resolution: true,
                },
                fuel_limit: None,
                time_limit_seconds: None,
                config: [
                    ("CHAIN".to_string(), ctx.chain_key().to_string()),
                    (
                        "SERVICE_HANDLER_CONTRACT_ADDRESS".to_string(),
                        contract_service_handler.address.clone(),
                    ),
                ]
                .into_iter()
                .collect(),
                env_keys: [
                    "WAVS_ENV_BACKEND_API_URL",
                    "WAVS_ENV_BACKEND_API_KEY",
                    "WAVS_ENV_IMAP_DEBUG_CAPABILITIES",
                    "WAVS_ENV_IMAP_PORT",
                    "WAVS_ENV_IMAP_HOST",
                    "WAVS_ENV_IMAP_TLS",
                    "WAVS_ENV_MAIL_CREDENTIAL_KIND",
                    "WAVS_ENV_IMAP_USERNAME",
                    "WAVS_ENV_IMAP_PASSWORD",
                    "WAVS_ENV_AGENTMAIL_API_KEY",
                    "WAVS_ENV_AGENTMAIL_INBOX_ID",
                    "WAVS_ENV_AGENTMAIL_BASE_URL",
                ]
                .into_iter()
                .map(|s| s.to_string())
                .collect(),
            };

            let aggregator_submitter_component = wavs_types::Component {
                source: ComponentSource::Download {
                    //uri: component_aggregator.uri.parse().unwrap(),
                    uri: component_aggregator_submitter.uri.parse().unwrap(),
                    digest: component_aggregator_submitter.digest,
                },
                permissions: wavs_types::Permissions {
                    allowed_http_hosts: wavs_types::AllowedHostPermission::All,
                    file_system: false,
                    raw_sockets: false,
                    dns_resolution: false,
                },
                fuel_limit: None,
                time_limit_seconds: None,
                config: [
                    (
                        "SERVICE_HANDLER_CONTRACT_ADDRESS".to_string(),
                        contract_service_handler.address.clone(),
                    ),
                    ("CHAIN".to_string(), ctx.chain_key().to_string()),
                ]
                .into_iter()
                .collect(),
                env_keys: ["WAVS_ENV_BACKEND_API_URL", "WAVS_ENV_BACKEND_API_KEY"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            };

            let submit_chain = Submit::Aggregator {
                component: Box::new(aggregator_submitter_component),
                signature_kind: SignatureKind::evm_default(),
            };

            let workflow = Workflow {
                trigger,
                component: operator_email_reader_component,
                submit: submit_chain,
            };

            let service = Service {
                name: "Hydro Email".to_string(),
                workflows: [("workflow-1".parse().unwrap(), workflow)]
                    .into_iter()
                    .collect(),
                status: if activate {
                    wavs_types::ServiceStatus::Active
                } else {
                    wavs_types::ServiceStatus::Paused
                },
                manager: ServiceManager::Cosmos {
                    chain: ctx.chain_key(),
                    address: middleware_instantiation
                        .service_manager_address
                        .parse()
                        .unwrap(),
                },
            };

            let bytes = serde_json::to_vec_pretty(&service).unwrap();

            let digest = wavs_types::ServiceDigest::hash(&bytes);

            let resp = IpfsFile::upload(
                ipfs_kind,
                bytes,
                "service.json",
                ipfs_gateway_url.as_ref(),
                ipfs_api_url.as_ref(),
                true,
            )
            .await
            .unwrap();

            let IpfsFile {
                cid,
                uri,
                gateway_url,
            } = resp;

            args.output()
                .write(OutputServiceUpload {
                    service,
                    digest,
                    cid,
                    uri: uri.clone(),
                    gateway_url: gateway_url.clone(),
                })
                .await
                .unwrap();

            println!("\nService URI: {}", uri);
            println!("Service Gateway URL: {}\n", gateway_url);
        }

        CliCommand::OperatorAddService {
            args: _,
            service_manager_address,
            wavs_url,
        } => {
            let req = wavs_types::AddServiceRequest {
                service_manager: ServiceManager::Cosmos {
                    chain: ctx.chain_key(),
                    address: service_manager_address.parse().unwrap(),
                },
            };

            let wavs_url = wavs_url.join("services").unwrap();

            let resp = reqwest::Client::new()
                .post(wavs_url.clone())
                .json(&req)
                .send()
                .await
                .unwrap();

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                panic!(
                    "Failed to add service at {}. Status: {}, Response: {}",
                    wavs_url, status, text
                );
            }
        }

        CliCommand::OperatorDeleteService {
            args: _,
            service_manager_address,
            wavs_url,
        } => {
            let req = wavs_types::DeleteServicesRequest {
                service_managers: vec![ServiceManager::Cosmos {
                    chain: ctx.chain_key(),
                    address: service_manager_address.parse().unwrap(),
                }],
            };

            let wavs_url = wavs_url.join("services").unwrap();

            let resp = reqwest::Client::new()
                .delete(wavs_url.clone())
                .json(&req)
                .send()
                .await
                .unwrap();

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                panic!(
                    "Failed to delete service at {}. Status: {}, Response: {}",
                    wavs_url, status, text
                );
            }
        }
        CliCommand::QueryServiceHandlerEmails {
            address,
            args: _,
            limit,
            start_after,
        } => {
            let address = ctx.parse_address(&address).await.unwrap();
            let client = ctx.service_handler_querier(address).await.unwrap();

            let emails = client.emails(limit, start_after).await.unwrap();

            println!("{:#?}\n", emails);

            println!("{} emails", emails.len());
        }
        CliCommand::QueryProxyConfig { address, args: _ } => {
            let address = ctx.parse_address(&address).await.unwrap();
            let client = ctx.proxy_querier(address).await.unwrap();

            let config = client.config().await.unwrap();

            println!("{:#?}\n", config);
        }
        CliCommand::QueryProxyState { address, args: _ } => {
            let address = ctx.parse_address(&address).await.unwrap();
            let client = ctx.proxy_querier(address).await.unwrap();

            let state = client.state().await.unwrap();

            println!("{:#?}\n", state);
        }
        CliCommand::ContractRegisterUser {
            email_address,
            user_registry_address,
            proxy_address,
            broadcast,
            ..
        } => {
            let proxy = ctx.parse_address(&proxy_address).await.unwrap();
            let msg = app_contract_api::user_registry::msg::ExecuteMsg::RegisterUser {
                user_id: UserId::new_email_address(&email_address),
                proxy_address: proxy.to_string(),
            };
            admin::register_or_prepare(
                &ctx,
                &user_registry_address,
                serde_json::to_value(msg).unwrap(),
                broadcast,
            )
            .await
            .unwrap();
        }
    }
}

fn strip_trailing_slash(url: &Url) -> String {
    let s = url.as_str();
    match s.strip_suffix('/') {
        Some(stripped) => stripped.to_string(),
        None => s.to_string(),
    }
}
