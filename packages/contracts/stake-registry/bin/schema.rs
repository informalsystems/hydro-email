use app_contract_api::stake_registry::{ExecuteMsg, InstantiateMsg, QueryMsg};
use cosmwasm_schema::write_api;

fn main() {
    write_api! { instantiate: InstantiateMsg, execute: ExecuteMsg, query: QueryMsg, };
}
