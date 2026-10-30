use app_client::{
    contracts::user_registry::{UserRegistryContract, UserRegistryExecutor, UserRegistryQuerier},
    executor::SigningClientWrapper,
};
use layer_climb::prelude::*;

use crate::code_ids::CodeId;

#[derive(Clone)]
pub struct UserRegistryClient {
    pub querier: UserRegistryQuerier,
    pub executor: UserRegistryExecutor,
    pub address: Address,
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

// For executing transactions with a stable admin address, we need to use the same client
// so it can't benefit from the pool which is totally fine, it will just have that specific wallet throughout
impl UserRegistryClient {
    pub async fn new(client: SigningClientWrapper, admins: Option<Vec<Address>>) -> Self {
        let msg = app_contract_api::user_registry::msg::InstantiateMsg {
            owner: client.addr.to_string(),
            admins: match admins {
                Some(admins) => admins.iter().map(|a| a.to_string()).collect(),
                None => vec![client.addr.to_string()],
            },
        };

        let (address, _) = client
            .contract_instantiate(
                None,
                CodeId::new_user_registry().await,
                "User Registry",
                &msg,
                vec![],
                None,
            )
            .await
            .unwrap();

        let querier =
            UserRegistryQuerier::new(client.querier.clone().into(), address.clone().into());
        let executor = UserRegistryExecutor::new(client.into(), address.clone().into());

        Self {
            querier,
            executor,
            address,
        }
    }
}
