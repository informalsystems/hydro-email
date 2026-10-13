use app_client::contracts::proxy::{ProxyContract, ProxyExecutor, ProxyQuerier};
use cosmwasm_std::Addr;
use cw_multi_test::{ContractWrapper, Executor};

use crate::client::AppClient;
use crate::mocks;

#[derive(Clone)]
pub struct ProxyClient {
    pub querier: ProxyQuerier,
    pub executor: ProxyExecutor,
    pub address: Addr,
}

impl From<ProxyClient> for ProxyContract {
    fn from(client: ProxyClient) -> Self {
        ProxyContract {
            querier: client.querier,
            executor: client.executor,
            address: client.address.into(),
        }
    }
}

impl ProxyClient {
    pub fn code_id(app_client: &AppClient) -> u64 {
        let contract = ContractWrapper::new(
            hydro_proxy::contract::execute,
            hydro_proxy::contract::instantiate,
            hydro_proxy::contract::query,
        )
        .with_reply(hydro_proxy::contract::reply);
        app_client.with_app_mut(|app| app.store_code(Box::new(contract)))
    }

    pub fn new(app_client: AppClient, code_id: u64, admins: Vec<Addr>) -> Self {
        let admins = if admins.is_empty() {
            vec![app_client.admin()]
        } else {
            admins
        };

        // Set up mock control center and vault for hydro proxy
        let control_center_addr = app_client.with_app_mut(|app| {
            // Store mock vault code
            let vault_code_id = app.store_code(mocks::vault::contract());

            // Store mock control center code
            let control_center_code_id = app.store_code(mocks::control_center::contract());

            // Instantiate mock control center first (vault needs it)
            let control_center_addr = app
                .instantiate_contract(
                    control_center_code_id,
                    app.api().addr_make("admin"),
                    &mocks::control_center::InstantiateMsg {
                        deposit_cap: cosmwasm_std::Uint128::new(1_000_000_000_000),
                        whitelist: vec![],
                        subvaults: vec![],
                        fee_config: None,
                    },
                    &[],
                    "control_center",
                    None,
                )
                .unwrap();

            // Instantiate mock vault
            let vault_addr = app
                .instantiate_contract(
                    vault_code_id,
                    app.api().addr_make("admin"),
                    &mocks::vault::InstantiateMsg {
                        deposit_denom: "utoken".to_string(),
                        subdenom: "utoken".to_string(),
                        token_metadata: mocks::vault::DenomMetadata {
                            exponent: 6,
                            display: "utoken".to_string(),
                            name: "utoken".to_string(),
                            description: "utoken".to_string(),
                            symbol: "UTOKEN".to_string(),
                            uri: None,
                            uri_hash: None,
                        },
                        control_center_contract: control_center_addr.to_string(),
                        token_info_provider_contract: None,
                        whitelist: vec![],
                        max_withdrawals_per_user: 10,
                    },
                    &[],
                    "vault",
                    None,
                )
                .unwrap();

            // Re-instantiate control center with vault as subvault
            let control_center_addr = app
                .instantiate_contract(
                    control_center_code_id,
                    app.api().addr_make("admin"),
                    &mocks::control_center::InstantiateMsg {
                        deposit_cap: cosmwasm_std::Uint128::new(1_000_000_000_000),
                        whitelist: vec![],
                        subvaults: vec![vault_addr.to_string()],
                        fee_config: None,
                    },
                    &[],
                    "control_center_with_vault",
                    None,
                )
                .unwrap();

            control_center_addr
        });

        let msg = hydro_proxy::msg::InstantiateMsg {
            admins: admins.into_iter().map(|x| x.to_string()).collect(),
            control_centers: vec![control_center_addr.to_string()],
        };

        let address = app_client.with_app_mut(|app| {
            app.instantiate_contract(code_id, app_client.admin(), &msg, &[], "proxy", None)
                .unwrap()
        });

        let querier = ProxyQuerier::new(app_client.querier.clone(), address.clone().into());
        let executor = ProxyExecutor::new(app_client.executor.clone(), address.clone().into());

        Self {
            querier,
            executor,
            address,
        }
    }
}
