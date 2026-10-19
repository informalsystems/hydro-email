use app_contract_api::{
    service_handler::msg::{
        Auth, CustomExecuteMsg, CustomQueryMsg, EmailsResponse, EventProcessedResponse, ExecuteMsg,
        InstantiateMsg, QueryMsg, UserIdEmail,
    },
    user_registry::msg::{ProxyAddressResponse, UserId},
};
use cosmwasm_std::{
    to_json_binary, Addr, Binary, Deps, DepsMut, Empty, Env, MessageInfo, Response, StdError,
    StdResult,
};
use cw_multi_test::{App, ContractWrapper, Executor};
use cw_storage_plus::Item;
use serde_json::{json, Value};
use wavs_types::{
    contracts::cosmwasm::{
        service_handler::{ServiceHandlerExecuteMessages, WavsEnvelope, WavsSignatureData},
        service_manager::{
            error::WavsValidateError, ServiceManagerQueryMessages, WavsValidateResult,
        },
    },
    Envelope,
};

const FAIL_PROXY: Item<bool> = Item::new("fail-proxy");
const EXECUTIONS: Item<u64> = Item::new("executions");

fn mock_instantiate(deps: DepsMut, _: Env, _: MessageInfo, _: Empty) -> StdResult<Response> {
    EXECUTIONS.save(deps.storage, &0)?;
    Ok(Response::new())
}

// One mock supplies service validation, registry lookup, and a fallible proxy.
fn mock_execute(deps: DepsMut, _: Env, _: MessageInfo, msg: Value) -> StdResult<Response> {
    if let Some(fail) = msg.get("fail_proxy").and_then(Value::as_bool) {
        FAIL_PROXY.save(deps.storage, &fail)?;
    } else {
        assert!(msg.get("forward_to_inflow").is_some());
        if FAIL_PROXY.may_load(deps.storage)?.unwrap_or(false) {
            return Err(StdError::msg("proxy failed"));
        }
        EXECUTIONS.update(deps.storage, |n| -> StdResult<_> { Ok(n + 1) })?;
    }
    Ok(Response::new())
}

fn mock_query(deps: Deps, env: Env, msg: Value) -> StdResult<Binary> {
    if msg.get("wavs_validate").is_some() {
        let ServiceManagerQueryMessages::WavsValidate { signature_data, .. } =
            cosmwasm_std::from_json(to_json_binary(&msg)?)?
        else {
            unreachable!()
        };
        to_json_binary(&if signature_data.reference_block == 0 {
            WavsValidateResult::Err(WavsValidateError::InsufficientQuorumZero)
        } else {
            WavsValidateResult::Ok
        })
    } else if msg.get("proxy_address").is_some() {
        to_json_binary(&ProxyAddressResponse {
            address: env.contract.address,
        })
    } else {
        assert!(msg.get("executions").is_some());
        to_json_binary(&EXECUTIONS.load(deps.storage)?)
    }
}

struct Suite {
    app: App,
    sender: Addr,
    mock: Addr,
    handler: Addr,
}

impl Suite {
    fn new() -> Self {
        let mut app = App::default();
        let sender = app.api().addr_make("relayer");
        let mock_code = app.store_code(Box::new(ContractWrapper::new(
            mock_execute,
            mock_instantiate,
            mock_query,
        )));
        let mock = app
            .instantiate_contract(mock_code, sender.clone(), &Empty {}, &[], "mock", None)
            .unwrap();
        let handler_code = app.store_code(Box::new(ContractWrapper::new(
            app_contract_service_handler::execute,
            app_contract_service_handler::instantiate,
            app_contract_service_handler::query,
        )));
        let handler = app
            .instantiate_contract(
                handler_code,
                sender.clone(),
                &InstantiateMsg {
                    auth: Auth::ServiceManager(mock.to_string()),
                    user_registry: mock.to_string(),
                },
                &[],
                "handler",
                None,
            )
            .unwrap();
        Self {
            app,
            sender,
            mock,
            handler,
        }
    }

    fn assert_counts(&self, expected: u64) {
        let executions: u64 = self
            .app
            .wrap()
            .query_wasm_smart(&self.mock, &json!({"executions": {}}))
            .unwrap();
        assert_eq!(executions, expected);
        let emails: EmailsResponse = self
            .app
            .wrap()
            .query_wasm_smart(
                &self.handler,
                &QueryMsg::Custom(CustomQueryMsg::Emails {
                    limit: None,
                    start_after: None,
                }),
            )
            .unwrap();
        assert_eq!(emails.emails.len() as u64, expected);
        for (index, (_, pagination_id)) in emails.emails.iter().enumerate() {
            assert_eq!(*pagination_id, index as u64 + 1);
        }
    }

    fn assert_event_processed(&self, id: u8, expected: bool) {
        let event: EventProcessedResponse = self
            .app
            .wrap()
            .query_wasm_smart(
                &self.handler,
                &QueryMsg::Custom(CustomQueryMsg::EventProcessed {
                    event_id: [id; 20].into(),
                }),
            )
            .unwrap();
        assert_eq!(event.processed, expected);
    }
}

fn message(id: u8, ordering: u8, subject: &str, reference_block: u32) -> ExecuteMsg {
    let payload = CustomExecuteMsg::Email(UserIdEmail {
        from: UserId::new_raw("user".to_string()),
        subject: subject.to_string(),
    })
    .encode()
    .unwrap();
    signed_payload(id, ordering, payload, reference_block)
}

fn signed_payload(id: u8, ordering: u8, payload: Vec<u8>, reference_block: u32) -> ExecuteMsg {
    ExecuteMsg::Wavs(ServiceHandlerExecuteMessages::WavsHandleSignedEnvelope {
        envelope: WavsEnvelope::new(Envelope {
            eventId: [id; 20].into(),
            ordering: [ordering; 12].into(),
            payload: payload.into(),
        }),
        signature_data: WavsSignatureData {
            signers: vec![],
            signatures: vec![],
            reference_block,
        },
    })
}

#[test]
fn replay_is_rejected_even_with_changed_envelope_or_relayer() {
    let mut suite = Suite::new();
    suite.assert_event_processed(1, false);
    suite.assert_event_processed(2, false);
    let msg = message(1, 0, "deposit", 1);
    suite
        .app
        .execute_contract(suite.sender.clone(), suite.handler.clone(), &msg, &[])
        .unwrap();
    suite.assert_event_processed(1, true);
    for replay in [msg, message(1, 1, "forward", 2)] {
        let other = suite.app.api().addr_make("other-relayer");
        let err = suite
            .app
            .execute_contract(other, suite.handler.clone(), &replay, &[])
            .unwrap_err();
        assert!(
            err.to_string().contains("Event already processed"),
            "{err:?}"
        );
        suite.assert_event_processed(1, true);
        suite.assert_counts(1);
    }
    // Identical actions from distinct events remain valid.
    suite
        .app
        .execute_contract(
            suite.sender.clone(),
            suite.handler.clone(),
            &message(2, 0, "deposit", 1),
            &[],
        )
        .unwrap();
    suite.assert_counts(2);
    suite.assert_event_processed(1, true);
    suite.assert_event_processed(2, true);
}

#[test]
fn event_status_query_rejects_invalid_id_lengths() {
    let suite = Suite::new();
    for len in [0, 19, 21] {
        let err = suite
            .app
            .wrap()
            .query_wasm_smart::<EventProcessedResponse>(
                &suite.handler,
                &json!({"event_processed": {"event_id": "01".repeat(len)}}),
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("Event ID must be 20 bytes"),
            "{err:?}"
        );
    }
}

#[test]
fn proxy_failure_rolls_back_marker_and_email_history() {
    let mut suite = Suite::new();
    suite
        .app
        .execute_contract(
            suite.sender.clone(),
            suite.mock.clone(),
            &json!({"fail_proxy": true}),
            &[],
        )
        .unwrap();
    let msg = message(1, 0, "deposit", 1);
    let err = suite
        .app
        .execute_contract(suite.sender.clone(), suite.handler.clone(), &msg, &[])
        .unwrap_err();
    assert!(err.to_string().contains("proxy failed"), "{err:?}");
    suite.assert_event_processed(1, false);
    suite.assert_counts(0);
    suite
        .app
        .execute_contract(
            suite.sender.clone(),
            suite.mock.clone(),
            &json!({"fail_proxy": false}),
            &[],
        )
        .unwrap();
    suite
        .app
        .execute_contract(suite.sender.clone(), suite.handler.clone(), &msg, &[])
        .unwrap();
    suite.assert_counts(1);
    suite.assert_event_processed(1, true);
}

#[test]
fn invalid_validation_or_payload_does_not_consume_event() {
    let mut suite = Suite::new();
    for (msg, expected) in [
        (message(1, 0, "deposit", 0), "Insufficient quorum"),
        (
            signed_payload(1, 0, b"invalid json".to_vec(), 1),
            "Payload decode",
        ),
    ] {
        let err = suite
            .app
            .execute_contract(suite.sender.clone(), suite.handler.clone(), &msg, &[])
            .unwrap_err();
        assert!(err.to_string().contains(expected), "{err:?}");
        suite.assert_event_processed(1, false);
        suite.assert_counts(0);
    }
    suite
        .app
        .execute_contract(
            suite.sender.clone(),
            suite.handler.clone(),
            &message(1, 0, "deposit", 1),
            &[],
        )
        .unwrap();
    suite.assert_counts(1);
    suite.assert_event_processed(1, true);
}
