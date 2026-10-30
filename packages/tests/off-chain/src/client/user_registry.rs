use app_client::contracts::user_registry::{
    UserRegistryContract, UserRegistryExecutor, UserRegistryQuerier,
};
use cosmwasm_std::Addr;
use cw_multi_test::{ContractWrapper, Executor};

use crate::client::AppClient;

#[derive(Clone)]
pub struct UserRegistryClient {
    pub querier: UserRegistryQuerier,
    pub executor: UserRegistryExecutor,
    pub address: Addr,
}

impl From<UserRegistryClient> for UserRegistryContract {
    fn from(client: UserRegistryClient) -> Self {
        UserRegistryContract {
            querier: client.querier,
            executor: client.executor,
            address: client.address.into(),
        }
    }
}

impl UserRegistryClient {
    pub fn new(app_client: AppClient) -> Self {
        let admin = app_client.admin();
        Self::new_with_admins(app_client, vec![admin])
    }

    pub fn new_with_admins(app_client: AppClient, admins: Vec<Addr>) -> Self {
        let contract = ContractWrapper::new(
            app_contract_user_registry::execute,
            app_contract_user_registry::instantiate,
            app_contract_user_registry::query,
        );
        let code_id = app_client.with_app_mut(|app| app.store_code(Box::new(contract)));

        let msg = app_contract_api::user_registry::msg::InstantiateMsg {
            owner: app_client.admin().to_string(),
            admins: admins.iter().map(|a| a.to_string()).collect(),
        };

        let address = app_client.with_app_mut(|app| {
            app.instantiate_contract(
                code_id,
                app_client.admin(),
                &msg,
                &[],
                "user registry",
                None,
            )
            .unwrap()
        });

        let querier = UserRegistryQuerier::new(app_client.querier.clone(), address.clone().into());
        let executor =
            UserRegistryExecutor::new(app_client.executor.clone(), address.clone().into());

        Self {
            querier,
            executor,
            address,
        }
    }
}
