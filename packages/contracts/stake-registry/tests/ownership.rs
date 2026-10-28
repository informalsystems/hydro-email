use app_contract_api::stake_registry::{ExecuteMsg, InstantiateMsg, QueryMsg, ValidationResult};
use app_contract_stake_registry::contract;
use cosmwasm_std::{
    to_json_binary, Addr, Binary, Deps, DepsMut, Empty, Env, MessageInfo, Response, StdResult,
    Uint256, WasmMsg,
};
use cw_multi_test::{App, ContractWrapper, Executor};
use cw_storage_plus::Item;
use layer_climb_address::EvmAddr;
use serde_json::{json, Value};
use wavs_types::{
    contracts::cosmwasm::service_handler::{WavsEnvelope, WavsSignatureData},
    Envelope,
};

// Only the manager's factory interface is needed here; production continues to
// use the pinned upstream service-manager WASM.
const MANAGER: Item<Value> = Item::new("manager");
fn manager_init(deps: DepsMut, _: Env, info: MessageInfo, msg: Value) -> StdResult<Response> {
    MANAGER.save(
        deps.storage,
        &json!({"admin": msg["admin"], "registry": info.sender}),
    )?;
    Ok(Response::new())
}
fn manager_execute(_: DepsMut, _: Env, _: MessageInfo, _: Empty) -> StdResult<Response> {
    Ok(Response::new())
}
fn manager_query(deps: Deps, _: Env, _: Empty) -> StdResult<Binary> {
    to_json_binary(&MANAGER.load(deps.storage)?)
}

fn setup() -> (App, Addr, Addr, Addr, Addr) {
    let mut app = App::default();
    let deployer = app.api().addr_make("deployer");
    let committee = app.api().addr_make("committee");
    let manager_code = app.store_code(Box::new(ContractWrapper::new(
        manager_execute,
        manager_init,
        manager_query,
    )));
    let registry_code = app.store_code(Box::new(ContractWrapper::new(
        contract::execute,
        contract::instantiate,
        contract::query,
    )));
    let registry = app
        .instantiate_contract(
            registry_code,
            deployer.clone(),
            &InstantiateMsg {
                owner: committee.to_string(),
                service_manager_instantiate: WasmMsg::Instantiate2 {
                    admin: None,
                    code_id: manager_code,
                    msg: to_json_binary(&json!({"admin": committee})).unwrap(),
                    funds: vec![],
                    label: "manager".to_owned(),
                    salt: Binary::from(b"service_manager"),
                },
            },
            &[],
            "registry",
            None,
        )
        .unwrap();
    let manager: Addr = app
        .wrap()
        .query_wasm_smart(&registry, &QueryMsg::GetServiceManager {})
        .unwrap();
    (app, deployer, committee, registry, manager)
}

fn address(n: u8) -> EvmAddr {
    EvmAddr::new([n; 20])
}

#[test]
fn explicit_owner_controls_keys_and_weights_and_factory_relationship_is_preserved() {
    let (mut app, deployer, committee, registry, manager) = setup();
    let stored: Value = app.wrap().query_wasm_smart(&manager, &Empty {}).unwrap();
    assert_eq!(stored, json!({"admin": committee, "registry": registry}));
    for contract in [&registry, &manager] {
        assert!(app
            .wrap()
            .query_wasm_contract_info(contract)
            .unwrap()
            .admin
            .is_none());
    }
    let update = ExecuteMsg::SetOperatorDetails {
        operator: address(1),
        signing_key: address(2),
        weight: Uint256::from(100u32),
    };
    assert!(app
        .execute_contract(deployer.clone(), registry.clone(), &update, &[])
        .is_err());
    app.execute_contract(committee.clone(), registry.clone(), &update, &[])
        .unwrap();
    let batch = ExecuteMsg::BatchSetOperatorDetails {
        operators: vec![address(1), address(3)],
        signing_keys: vec![address(4), address(5)],
        weights: vec![Uint256::zero(), Uint256::from(200u32)],
    };
    assert!(app
        .execute_contract(deployer, registry.clone(), &batch, &[])
        .is_err());
    app.execute_contract(committee, registry.clone(), &batch, &[])
        .unwrap();
    let weight: Uint256 = app
        .wrap()
        .query_wasm_smart(&registry, &QueryMsg::GetTotalWeight {})
        .unwrap();
    assert_eq!(weight, Uint256::from(200u32));
    let key: Option<EvmAddr> = app
        .wrap()
        .query_wasm_smart(
            &registry,
            &QueryMsg::GetOperatorSigningKey {
                operator: address(1),
            },
        )
        .unwrap();
    assert_eq!(key, Some(address(4)));
    let previous: Option<EvmAddr> = app
        .wrap()
        .query_wasm_smart(
            &registry,
            &QueryMsg::GetLatestOperatorForSigningKey {
                signing_key: address(2),
            },
        )
        .unwrap();
    assert_eq!(previous, None);
}

#[test]
fn validates_real_signatures_and_rejects_duplicates_tampering_and_unregistered_keys() {
    use alloy_primitives::{eip191_hash_message, keccak256};
    use k256::ecdsa::SigningKey;
    let (mut app, _, committee, registry, _) = setup();
    let key = SigningKey::from_bytes((&[7u8; 32]).into()).unwrap();
    let encoded = key.verifying_key().to_encoded_point(false);
    let signer = EvmAddr::new(contract::ethereum_address_raw(encoded.as_bytes()).unwrap());
    app.execute_contract(
        committee,
        registry.clone(),
        &ExecuteMsg::SetOperatorDetails {
            operator: address(1),
            signing_key: signer.clone(),
            weight: Uint256::from(100u32),
        },
        &[],
    )
    .unwrap();
    let envelope = WavsEnvelope::new(Envelope {
        eventId: [1; 20].into(),
        ordering: [0; 12].into(),
        payload: vec![1, 2, 3].into(),
    });
    let digest = eip191_hash_message(keccak256(envelope.as_slice()));
    let (sig, recovery) = key.sign_prehash_recoverable(digest.as_slice()).unwrap();
    let mut bytes = sig.to_bytes().to_vec();
    bytes.push(recovery.to_byte());
    let signature_data = WavsSignatureData {
        signers: vec![signer.clone()],
        signatures: vec![bytes.into()],
        reference_block: app.block_info().height as u32,
    };
    let query = QueryMsg::ValidateSignature {
        envelope: envelope.clone(),
        signature_data: signature_data.clone(),
    };
    let result: ValidationResult = app.wrap().query_wasm_smart(&registry, &query).unwrap();
    assert_eq!(result.total_voting_power, Uint256::from(100u32));
    assert_eq!(result.voting_power_signed, Uint256::from(100u32));
    let mut duplicate = signature_data.clone();
    duplicate.signers.push(signer);
    duplicate.signatures.push(duplicate.signatures[0].clone());
    assert!(app
        .wrap()
        .query_wasm_smart::<ValidationResult>(
            &registry,
            &QueryMsg::ValidateSignature {
                envelope: envelope.clone(),
                signature_data: duplicate
            }
        )
        .is_err());
    let changed = WavsEnvelope::new(Envelope {
        eventId: [2; 20].into(),
        ordering: [0; 12].into(),
        payload: vec![1, 2, 3].into(),
    });
    assert!(app
        .wrap()
        .query_wasm_smart::<ValidationResult>(
            &registry,
            &QueryMsg::ValidateSignature {
                envelope: changed,
                signature_data: signature_data.clone()
            }
        )
        .is_err());
    let mut unknown = signature_data;
    unknown.signers[0] = address(9);
    assert!(app
        .wrap()
        .query_wasm_smart::<ValidationResult>(
            &registry,
            &QueryMsg::ValidateSignature {
                envelope,
                signature_data: unknown
            }
        )
        .is_err());
}
