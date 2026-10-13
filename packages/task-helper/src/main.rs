mod command;
mod config;
mod context;
mod ipfs;
mod output;

use std::process::exit;

use app_client::contracts::user_registry::UserRegistryContract;
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
        OutputComponentUpload, OutputContractInstantiate, OutputContractUpload,
        OutputOperatorSetSigningKey, OutputServiceUpload,
    },
};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // Install rustls crypto provider before any TLS operations
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    tracing_init();

    let ctx = CliContext::new().await;

    match ctx.command.clone() {
        CliCommand::OperatorSetSigningKey {
            service_manager_address,
            evm_operator_address,
            stake_registry_address,
            weight,
            wavs_url,
            args,
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

            let client = ctx.signing_client().await.unwrap();

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

            // Create the SetOperatorDetails message
            // https://github.com/Lay3rLabs/cw-middleware/blob/abf177f139c0e960ab0bdfaa8bbe37c67ba6ce00/packages/contracts/mirror/api/src/stake_registry.rs
            // TODO: move this to middleware docker cli
            let set_operator_details_msg = serde_json::json!({
                "set_operator_details": {
                    "operator": evm_operator_address.to_string(),
                    "signing_key": evm_signing_key_address.to_string(),
                    "weight": weight_uint.to_string()
                }
            });

            let tx_resp = client
                .contract_execute(
                    &stake_registry_address,
                    &set_operator_details_msg,
                    vec![],
                    None,
                )
                .await
                .unwrap();

            let stake_registry_tx_hash = tx_resp.txhash;

            args.output()
                .write(OutputOperatorSetSigningKey {
                    stake_registry_tx_hash,
                    evm_operator_address,
                    evm_signing_key_address,
                })
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
        CliCommand::InstantiateUserRegistry { args, code_id } => {
            let client = ctx.signing_client().await.unwrap();

            let instantiate_msg = app_contract_api::user_registry::msg::InstantiateMsg {
                admins: vec![ctx.wallet_addr().await.unwrap().to_string()],
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
                "subvaults": []
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
        CliCommand::InstantiateProxy {
            admins,
            control_centers,
            args,
            code_id,
        } => {
            let client = ctx.signing_client().await.unwrap();

            let instantiate_msg = hydro_proxy::msg::InstantiateMsg {
                admins,
                control_centers,
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
                config: Default::default(),
                env_keys: [
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
                env_keys: Default::default(),
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
            args: _,
        } => {
            let client = ctx.signing_client().await.unwrap();

            let user_registry_address = ctx.parse_address(&user_registry_address).await.unwrap();

            let proxy_address = ctx.parse_address(&proxy_address).await.unwrap();

            let contract = UserRegistryContract::new(
                client.querier.clone().into(),
                client.into(),
                user_registry_address.into(),
            );

            let user_id = UserId::new_email_address(&email_address);

            let (tx_resp, user_id) = contract
                .executor
                .register_user_id(user_id, proxy_address.clone().into())
                .await
                .unwrap();

            println!("Registered user");
            println!("TX Hash: {}", tx_resp.unchecked_into_tx_response().txhash);
            println!("Proxy address: {}", proxy_address);
            println!("Email address: {}", email_address);
            println!("User ID: {}", user_id);
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
