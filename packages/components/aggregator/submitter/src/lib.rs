use layer_climb::prelude::CosmosAddr;

mod backend;
mod reporting;

use crate::wavs::aggregator::output::{CosmosAddress, CosmosSubmitAction, SubmitAction};

wit_bindgen::generate!({
    path: "../../../../wit-definitions/aggregator/wit",
    world: "aggregator-world",
    generate_all,
    with: {
        "wasi:io/poll@0.2.0": wasip2::io::poll
    },
    features: ["tls"]
});

struct Component;

impl Guest for Component {
    fn process_input(input: AggregatorInput) -> Result<Vec<AggregatorAction>, String> {
        let chain = host::config_var("CHAIN").ok_or("CHAIN config var is required")?;
        let service_handler_addr = host::config_var("SERVICE_HANDLER_CONTRACT_ADDRESS")
            .ok_or("SERVICE_HANDLER_CONTRACT_ADDRESS config var is required")?;

        host::get_cosmos_chain_config(&chain)
            .ok_or(format!("failed to get chain config for {}", chain))?;

        let service_handler_addr =
            CosmosAddr::new_str(&service_handler_addr, None).map_err(|e| e.to_string())?;

        let report = reporting::ActionReport::new(
            chain.clone(),
            service_handler_addr.to_string(),
            &host::get_event_id(),
            &input.operator_response.payload,
        )
        .map_err(|error| error.to_string())?;
        let backend = backend::BackendClient::from_env().map_err(|error| error.to_string())?;
        let event_id = report.event_id.clone();
        let submit = wstd::runtime::block_on(async move { report.prepare(&backend).await })
            .map_err(|error| {
                host::log(
                    wavs::types::core::LogLevel::Error,
                    &format!(
                        "Action {}: pre-submission reporting failed: {error}",
                        event_id
                    ),
                );
                error.to_string()
            })?;
        if !submit {
            return Ok(vec![]);
        }

        Ok(vec![AggregatorAction::Submit(SubmitAction::Cosmos(
            CosmosSubmitAction {
                chain: chain.to_string(),
                address: CosmosAddress {
                    bech32_addr: service_handler_addr.to_string(),
                    prefix_len: service_handler_addr.prefix().len() as u32,
                },
                gas_price: None,
            },
        ))])
    }

    fn handle_timer_callback(_input: AggregatorInput) -> Result<Vec<AggregatorAction>, String> {
        Ok(vec![])
    }

    fn handle_submit_callback(
        input: AggregatorInput,
        tx_result: Result<AnyTxHash, String>,
    ) -> Result<(), String> {
        let chain = host::config_var("CHAIN").ok_or("CHAIN config var is required")?;
        let handler = host::config_var("SERVICE_HANDLER_CONTRACT_ADDRESS")
            .ok_or("SERVICE_HANDLER_CONTRACT_ADDRESS config var is required")?;
        let report = reporting::ActionReport::new(
            chain,
            handler,
            &host::get_event_id(),
            &input.operator_response.payload,
        )
        .map_err(|error| error.to_string())?;
        let outcome = match tx_result {
            Ok(AnyTxHash::Cosmos(hash)) => reporting::CallbackOutcome::cosmos(Ok(hash)),
            Ok(AnyTxHash::Evm(_)) => reporting::CallbackOutcome::Unknown,
            Err(error) => reporting::CallbackOutcome::cosmos(Err(error)),
        };
        let backend = backend::BackendClient::from_env().map_err(|error| error.to_string())?;
        let event_id = report.event_id.clone();
        wstd::runtime::block_on(async move { report.callback(&backend, outcome).await }).map_err(
            |error| {
                host::log(
                    wavs::types::core::LogLevel::Error,
                    &format!(
                        "Action {}: callback reporting failed; reconciliation required: {error}",
                        event_id
                    ),
                );
                error.to_string()
            },
        )
    }
}

export!(Component);
