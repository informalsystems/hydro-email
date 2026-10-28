use crate::{config::path_builds, output::OutputFormat};
use clap::{Parser, ValueEnum};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, str::FromStr};
use wavs_types::ChainKey;

#[derive(Clone, Parser)]
#[command(version, about, long_about = None)]
#[allow(clippy::large_enum_variant)]
pub enum CliCommand {
    /// Upload a contract to the chain
    UploadContract {
        #[arg(long)]
        kind: ContractKind,

        #[clap(flatten)]
        args: CliArgs,
    },
    /// Instantiate the ServiceHandler contract
    InstantiateServiceHandler {
        #[arg(long)]
        code_id: u64,

        /// If AuthKind is User, then this is the user address
        /// and None means the CLI mnemonic address
        /// Otherwise it is the service manager address and must be supplied
        #[arg(long)]
        auth_address: Option<String>,

        #[arg(long, default_value_t = AuthKind::ServiceManager)]
        auth_kind: AuthKind,

        #[arg(long)]
        user_registry_address: String,

        #[clap(flatten)]
        args: CliArgs,
    },
    /// Instantiate the ServiceHandler contract
    InstantiateUserRegistry {
        #[arg(long)]
        code_id: u64,

        #[clap(flatten)]
        args: CliArgs,
    },
    /// Instantiate a local Hydro control center with no vaults
    InstantiateLocalControlCenter {
        #[arg(long)]
        code_id: u64,

        #[clap(flatten)]
        args: CliArgs,
    },
    /// Instantiate the shared Hydro control-center registry
    InstantiateControlCenterRegistry {
        #[arg(long)]
        code_id: u64,

        #[arg(long)]
        /// Required on remote chains; local development defaults to the signer.
        admin: Option<String>,

        #[arg(long, required = true, num_args = 1.., value_delimiter = ' ')]
        control_centers: Vec<String>,

        #[clap(flatten)]
        args: CliArgs,
    },
    /// Instantiate the Proxy contract
    InstantiateProxy {
        #[arg(long)]
        code_id: u64,

        #[arg(long, required = true, num_args = 1.., value_delimiter = ' ')]
        admins: Vec<String>,

        #[arg(long)]
        control_center_registry: String,

        #[clap(flatten)]
        args: CliArgs,
    },
    /// Upload a component to IPFS
    UploadComponent {
        #[arg(long)]
        kind: ComponentKind,

        #[arg(long)]
        component: String,

        #[arg(long)]
        ipfs_kind: IpfsKind,

        #[arg(long)]
        ipfs_api_url: Url,

        #[arg(long)]
        ipfs_gateway_url: Url,

        #[clap(flatten)]
        args: CliArgs,
    },
    /// Generate and Upload a service to IPFS
    UploadService {
        #[arg(long)]
        contract_service_handler_instantiation_file: PathBuf,

        #[arg(long)]
        middleware_instantiation_file: PathBuf,

        #[arg(long)]
        component_operator_email_reader_cid_file: PathBuf,

        #[arg(long)]
        component_aggregator_submitter_cid_file: PathBuf,

        #[arg(long)]
        trigger_cron_schedule: String,

        #[arg(long)]
        ipfs_kind: IpfsKind,

        #[arg(long)]
        ipfs_api_url: Url,

        #[arg(long)]
        ipfs_gateway_url: Url,

        #[arg(long)]
        activate: bool,

        #[clap(flatten)]
        args: CliArgs,
    },
    FaucetTap {
        /// if not supplied, will be the one in CLI_MNEMONIC
        addr: Option<String>,
        /// if not supplied, will be the default
        amount: Option<u128>,
        /// if not supplied, will be the default
        denom: Option<String>,
        #[clap(flatten)]
        args: CliArgs,
    },
    AssertAccountExists {
        addr: Option<String>,
        #[clap(flatten)]
        args: CliArgs,
    },
    OperatorAddService {
        #[arg(long)]
        service_manager_address: String,

        #[arg(long)]
        wavs_url: Url,

        #[clap(flatten)]
        args: CliArgs,
    },
    OperatorDeleteService {
        #[arg(long)]
        service_manager_address: String,

        #[arg(long)]
        wavs_url: Url,

        #[clap(flatten)]
        args: CliArgs,
    },
    OperatorSetSigningKey {
        /// The address of the service manager contract
        #[arg(long)]
        service_manager_address: String,

        /// The stake registry address
        #[arg(long)]
        stake_registry_address: String,

        /// The operator address (EVM address)
        #[arg(long)]
        evm_operator_address: String,

        /// The weight for the signing key
        #[arg(long)]
        weight: String,

        #[arg(long)]
        wavs_url: Url,

        #[clap(flatten)]
        args: CliArgs,
    },
    QueryServiceHandlerEmails {
        /// The address of the service handler contract
        #[arg(long)]
        address: String,

        /// The maximum number of emails to return
        #[arg(long)]
        limit: Option<u32>,

        /// Cursor for pagination
        #[arg(long)]
        start_after: Option<u64>,

        #[clap(flatten)]
        args: CliArgs,
    },
    QueryProxyConfig {
        /// The address of the proxy contract
        #[arg(long)]
        address: String,

        #[clap(flatten)]
        args: CliArgs,
    },
    QueryProxyState {
        /// The address of the proxy contract
        #[arg(long)]
        address: String,

        #[clap(flatten)]
        args: CliArgs,
    },
    ContractRegisterUser {
        #[arg(long)]
        email_address: String,

        #[arg(long)]
        user_registry_address: String,

        #[arg(long)]
        proxy_address: String,

        #[clap(flatten)]
        args: CliArgs,
    },
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HexBytes(Vec<u8>);

impl AsRef<[u8]> for HexBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl FromStr for HexBytes {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        const_hex::decode(s)
            .map(HexBytes)
            .map_err(|e| format!("invalid hex: {e}"))
    }
}

// common args for several commands
#[derive(Clone, Debug, Parser)]
pub struct CliArgs {
    #[clap(long)]
    pub chain: Option<ChainKey>,

    /// Filename for outputting any generated files
    /// which will be written in to the deployments directory
    #[clap(long)]
    pub output_file: Option<String>,

    /// Output format for any generated files
    #[clap(long, value_enum, default_value_t = OutputFormat::Json)]
    pub output_format: OutputFormat,
}

#[derive(Serialize, Deserialize, Debug, Clone, ValueEnum)]
#[clap(rename_all = "kebab-case")]
#[serde(rename_all = "kebab-case")]
pub enum ContractKind {
    ServiceHandler,
    Proxy,
    UserRegistry,
    LocalControlCenter,
    ControlCenterRegistry,
}

impl std::fmt::Display for ContractKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl ContractKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::ServiceHandler => "service-handler",
            Self::Proxy => "proxy",
            Self::UserRegistry => "user-registry",
            Self::LocalControlCenter => "local-control-center",
            Self::ControlCenterRegistry => "control-center-registry",
        }
    }
    pub async fn wasm_bytes(&self) -> Vec<u8> {
        let filename = self.as_str().replace('-', "_");
        let path = path_builds()
            .join("contracts")
            .join(format!("app_contract_{filename}.wasm"));

        tokio::fs::read(&path)
            .await
            .unwrap_or_else(|_| panic!("Failed to read wasm bytes at: {}", path.to_string_lossy()))
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, ValueEnum)]
#[clap(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    User,
    ServiceManager,
}

impl std::fmt::Display for AuthKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl AuthKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::User => "user",
            Self::ServiceManager => "service_manager",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, ValueEnum)]
#[clap(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum IpfsKind {
    Kubo,
    Pinata,
}

impl std::fmt::Display for IpfsKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl IpfsKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Kubo => "kubo",
            Self::Pinata => "pinata",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, ValueEnum)]
#[clap(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    Operator,
    Aggregator,
}

impl std::fmt::Display for ComponentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl ComponentKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Operator => "operator",
            Self::Aggregator => "aggregator",
        }
    }
    pub async fn wasm_bytes(&self, name: &str) -> Vec<u8> {
        let filename = name.replace('-', "_");
        let path = path_builds()
            .join("components")
            .join(format!("app_component_{}_{filename}.wasm", self.as_str()));

        tokio::fs::read(&path)
            .await
            .unwrap_or_else(|_| panic!("Failed to read wasm bytes at: {}", path.to_string_lossy()))
    }
}
