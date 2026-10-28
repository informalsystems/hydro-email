use app_client::{
    contracts::proxy::{ProxyContract, ProxyExecutor, ProxyQuerier},
    executor::SigningClientWrapper,
};
use layer_climb::prelude::*;

use crate::code_ids::CodeId;

#[derive(Clone)]
pub struct ProxyClient {
    pub querier: ProxyQuerier,
    pub executor: ProxyExecutor,
    pub address: Address,
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

// For stable addresses with instantiate2, we need to use the same client
// so it can't benefit from the pool which is totally fine, ProxyClient will just have that specific wallet throughout
impl ProxyClient {
    pub async fn new(
        client: SigningClientWrapper,
        admins: Option<Vec<Address>>,
        control_centers: Vec<Address>,
    ) -> Self {
        let (registry, _) = client
            .contract_instantiate(
                None,
                CodeId::new_control_center_registry().await,
                "Control Center Registry",
                &hydro_control_center_registry::msg::InstantiateMsg {
                    admin: client.addr.to_string(),
                    control_centers: control_centers.iter().map(ToString::to_string).collect(),
                },
                vec![],
                None,
            )
            .await
            .unwrap();
        let msg = hydro_proxy::msg::InstantiateMsg {
            admins: match admins {
                Some(admins) => admins.iter().map(|a| a.to_string()).collect(),
                None => vec![client.addr.to_string()],
            },
            control_center_registry: registry.to_string(),
        };

        let (address, _) = client
            .contract_instantiate(None, CodeId::new_proxy().await, "Proxy", &msg, vec![], None)
            .await
            .unwrap();

        let querier = ProxyQuerier::new(client.querier.clone().into(), address.clone().into());
        let executor = ProxyExecutor::new(client.into(), address.clone().into());

        Self {
            querier,
            executor,
            address,
        }
    }
}
