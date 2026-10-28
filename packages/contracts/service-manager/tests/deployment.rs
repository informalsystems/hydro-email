use app_contract_api::{
    service_manager::{ExecuteMsg, InstantiateMsg, MirrorServiceManagerQueryMessages, QueryMsg},
    stake_registry,
};
use app_contract_service_manager::contract;
use cosmwasm_std::{to_json_binary, Addr, Binary, CosmosMsg, Uint256, WasmMsg};
use cw_multi_test::{App, ContractWrapper, Executor};
use wavs_types::contracts::cosmwasm::service_manager::{
    QuorumThreshold, ServiceManagerExecuteMessages as WavsExecute,
    ServiceManagerQueryMessages as WavsQuery,
};

fn setup() -> (App, Addr, Addr, Addr, Addr) {
    let mut app = App::default();
    let deployer = app.api().addr_make("deployer");
    let committee = app.api().addr_make("committee");
    let manager_code = app.store_code(Box::new(ContractWrapper::new(
        contract::execute,
        contract::instantiate,
        contract::query,
    )));
    let registry_code = app.store_code(Box::new(ContractWrapper::new(
        app_contract_stake_registry::execute,
        app_contract_stake_registry::instantiate,
        app_contract_stake_registry::query,
    )));
    let registry = app
        .instantiate_contract(
            registry_code,
            deployer.clone(),
            &stake_registry::InstantiateMsg {
                owner: deployer.to_string(),
                service_manager_instantiate: WasmMsg::Instantiate2 {
                    admin: None,
                    code_id: manager_code,
                    msg: to_json_binary(&InstantiateMsg {
                        admin: deployer.to_string(),
                    })
                    .unwrap(),
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
        .query_wasm_smart(&registry, &stake_registry::QueryMsg::GetServiceManager {})
        .unwrap();
    (app, deployer, committee, manager, registry)
}

fn uri(value: &str) -> ExecuteMsg {
    ExecuteMsg::Wavs(WavsExecute::WavsSetServiceUri {
        service_uri: value.to_owned(),
    })
}
fn quorum(n: u32, d: u32) -> ExecuteMsg {
    ExecuteMsg::Wavs(WavsExecute::WavsSetQuorumThreshold {
        numerator: n.into(),
        denominator: d.into(),
    })
}
fn admin(app: &App, manager: &Addr) -> Addr {
    app.wrap()
        .query_wasm_smart(
            manager,
            &QueryMsg::Mirror(MirrorServiceManagerQueryMessages::Admin {}),
        )
        .unwrap()
}
fn execute_msg(manager: &Addr, msg: ExecuteMsg) -> CosmosMsg {
    WasmMsg::Execute {
        contract_addr: manager.to_string(),
        msg: to_json_binary(&msg).unwrap(),
        funds: vec![],
    }
    .into()
}

fn owner(app: &App, registry: &Addr) -> Addr {
    app.wrap()
        .query_wasm_smart(registry, &stake_registry::QueryMsg::Owner {})
        .unwrap()
}
fn registry_msg(registry: &Addr, msg: stake_registry::ExecuteMsg) -> CosmosMsg {
    WasmMsg::Execute {
        contract_addr: registry.to_string(),
        msg: to_json_binary(&msg).unwrap(),
        funds: vec![],
    }
    .into()
}

#[test]
fn cli_completes_setup_then_transfers_both_roles_without_committee_transactions() {
    use layer_climb_address::EvmAddr;
    let (mut app, deployer, committee, manager, registry) = setup();
    let recorded_registry: Addr = app
        .wrap()
        .query_wasm_smart(
            &manager,
            &QueryMsg::Mirror(MirrorServiceManagerQueryMessages::StakeRegistry {}),
        )
        .unwrap();
    assert_eq!(recorded_registry, registry);
    for contract in [&manager, &registry] {
        assert!(app
            .wrap()
            .query_wasm_contract_info(contract)
            .unwrap()
            .admin
            .is_none());
    }
    let transfer_manager = ExecuteMsg::TransferAdmin {
        new_admin: committee.to_string(),
    };
    let transfer_registry = stake_registry::ExecuteMsg::TransferOwnership {
        new_owner: committee.to_string(),
    };
    assert!(app
        .execute_contract(committee.clone(), manager.clone(), &transfer_manager, &[])
        .is_err());
    assert!(app
        .execute_contract(committee.clone(), registry.clone(), &transfer_registry, &[])
        .is_err());
    app.execute_multi(
        deployer.clone(),
        vec![
            execute_msg(&manager, uri("ipfs://initial/service.json")),
            execute_msg(&manager, quorum(2, 3)),
        ],
    )
    .unwrap();
    assert_eq!(admin(&app, &manager), deployer);
    assert_eq!(owner(&app, &registry), deployer);
    let operator = stake_registry::ExecuteMsg::SetOperatorDetails {
        operator: EvmAddr::new([1; 20]),
        signing_key: EvmAddr::new([2; 20]),
        weight: Uint256::from(100u32),
    };
    app.execute_contract(deployer.clone(), registry.clone(), &operator, &[])
        .unwrap();
    app.execute_multi(
        deployer.clone(),
        vec![
            execute_msg(&manager, transfer_manager),
            registry_msg(&registry, transfer_registry),
        ],
    )
    .unwrap();
    assert_eq!(admin(&app, &manager), committee);
    assert_eq!(owner(&app, &registry), committee);
    let stored_uri: String = app
        .wrap()
        .query_wasm_smart(&manager, &QueryMsg::Wavs(WavsQuery::WavsServiceUri {}))
        .unwrap();
    assert_eq!(stored_uri, "ipfs://initial/service.json");
    let threshold: QuorumThreshold = app
        .wrap()
        .query_wasm_smart(&manager, &QueryMsg::Wavs(WavsQuery::WavsQuorumThreshold {}))
        .unwrap();
    assert_eq!(
        (threshold.numerator, threshold.denominator),
        (Uint256::from(2u32), Uint256::from(3u32))
    );
    for msg in [
        uri("ipfs://unauthorized"),
        quorum(1, 2),
        ExecuteMsg::TransferAdmin {
            new_admin: deployer.to_string(),
        },
    ] {
        assert!(app
            .execute_contract(deployer.clone(), manager.clone(), &msg, &[])
            .is_err());
    }
    assert!(app
        .execute_contract(deployer.clone(), registry.clone(), &operator, &[])
        .is_err());
    assert!(app
        .execute_contract(
            deployer,
            registry.clone(),
            &stake_registry::ExecuteMsg::TransferOwnership {
                new_owner: committee.to_string()
            },
            &[]
        )
        .is_err());
    app.execute_contract(committee.clone(), registry, &operator, &[])
        .unwrap();
    app.execute_contract(
        committee.clone(),
        manager.clone(),
        &uri("ipfs://next/service.json"),
        &[],
    )
    .unwrap();
    app.execute_contract(committee.clone(), manager.clone(), &quorum(3, 4), &[])
        .unwrap();
    let next = app.api().addr_make("next-committee");
    app.execute_contract(
        committee,
        manager.clone(),
        &ExecuteMsg::TransferAdmin {
            new_admin: next.to_string(),
        },
        &[],
    )
    .unwrap();
    assert_eq!(admin(&app, &manager), next);
}

#[test]
fn invalid_registry_transfer_rolls_back_the_manager_handoff() {
    let (mut app, deployer, committee, manager, registry) = setup();
    app.execute_contract(
        deployer.clone(),
        manager.clone(),
        &uri("ipfs://original"),
        &[],
    )
    .unwrap();
    assert!(app
        .execute_multi(
            deployer.clone(),
            vec![
                execute_msg(
                    &manager,
                    ExecuteMsg::TransferAdmin {
                        new_admin: committee.to_string()
                    }
                ),
                registry_msg(
                    &registry,
                    stake_registry::ExecuteMsg::TransferOwnership {
                        new_owner: "invalid-address".to_owned()
                    }
                ),
            ]
        )
        .is_err());
    assert_eq!(admin(&app, &manager), deployer);
    assert_eq!(owner(&app, &registry), deployer);
    let stored: String = app
        .wrap()
        .query_wasm_smart(&manager, &QueryMsg::Wavs(WavsQuery::WavsServiceUri {}))
        .unwrap();
    assert_eq!(stored, "ipfs://original");
}

#[test]
fn local_manager_and_registry_preserve_wavs_signature_and_quorum_interfaces() {
    use alloy_primitives::{eip191_hash_message, keccak256};
    use k256::ecdsa::SigningKey;
    use layer_climb_address::EvmAddr;
    use wavs_types::{
        contracts::cosmwasm::{
            service_handler::{WavsEnvelope, WavsSignatureData},
            service_manager::WavsValidateResult,
        },
        Envelope,
    };
    let (mut app, deployer, committee, manager, registry) = setup();
    let envelope = WavsEnvelope::new(Envelope {
        eventId: [1; 20].into(),
        ordering: [0; 12].into(),
        payload: vec![1, 2, 3].into(),
    });
    let mut data = WavsSignatureData {
        signers: vec![],
        signatures: vec![],
        reference_block: app.block_info().height as u32,
    };
    for seed in [7u8, 8] {
        let key = SigningKey::from_bytes((&[seed; 32]).into()).unwrap();
        let signer = EvmAddr::new(
            app_contract_stake_registry::contract::ethereum_address_raw(
                key.verifying_key().to_encoded_point(false).as_bytes(),
            )
            .unwrap(),
        );
        app.execute_contract(
            deployer.clone(),
            registry.clone(),
            &stake_registry::ExecuteMsg::SetOperatorDetails {
                operator: signer.clone(),
                signing_key: signer.clone(),
                weight: Uint256::from(100u32),
            },
            &[],
        )
        .unwrap();
        let (signature, recovery) = key
            .sign_prehash_recoverable(
                eip191_hash_message(keccak256(envelope.as_slice())).as_slice(),
            )
            .unwrap();
        let mut bytes = signature.to_bytes().to_vec();
        bytes.push(recovery.to_byte());
        data.signers.push(signer);
        data.signatures.push(bytes.into());
    }
    app.execute_multi(
        deployer,
        vec![
            execute_msg(
                &manager,
                ExecuteMsg::TransferAdmin {
                    new_admin: committee.to_string(),
                },
            ),
            registry_msg(
                &registry,
                stake_registry::ExecuteMsg::TransferOwnership {
                    new_owner: committee.to_string(),
                },
            ),
        ],
    )
    .unwrap();
    let both: WavsValidateResult = app
        .wrap()
        .query_wasm_smart(
            &manager,
            &QueryMsg::Wavs(WavsQuery::WavsValidate {
                envelope: envelope.clone(),
                signature_data: data.clone(),
            }),
        )
        .unwrap();
    assert!(matches!(both, WavsValidateResult::Ok));
    data.signers.pop();
    data.signatures.pop();
    let one: WavsValidateResult = app
        .wrap()
        .query_wasm_smart(
            &manager,
            &QueryMsg::Wavs(WavsQuery::WavsValidate {
                envelope,
                signature_data: data,
            }),
        )
        .unwrap();
    assert!(matches!(one, WavsValidateResult::Err(_)));
}
