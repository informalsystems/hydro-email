#![allow(warnings)]
mod backend;
mod config;
mod email;
mod error;

use anyhow::bail;
use app_contract_api::{
    proxy::ProxyExecuteMsg,
    service_handler::msg::{CustomExecuteMsg, UserIdEmail},
    user_registry::msg::UserId,
};
use cfdkim::verify_email_with_resolver;

use crate::{email::verify::verify_email, wavs::operator::input::TriggerData};

// this is needed just to make the ide/compiler happy... we're _always_ compiling to wasm32-wasi
wit_bindgen::generate!({
    path: "../../../../wit-definitions/operator/wit",
    world: "wavs-world",
    generate_all,
    with: {
        "wasi:io/poll@0.2.0": wasip2::io::poll
    },
    features: ["tls"]
});

struct Component;

impl Guest for Component {
    fn run(trigger_action: TriggerAction) -> Result<Vec<WasmResponse>, String> {
        wstd::runtime::block_on(async move {
            match inner(trigger_action).await {
                Ok(resp) => Ok(resp),
                Err(e) => {
                    eprintln!("{:?}", e);
                    Err(e.to_string())
                }
            }
        })
    }
}

async fn inner(trigger_action: TriggerAction) -> anyhow::Result<Vec<WasmResponse>> {
    match trigger_action.data {
        TriggerData::Cron(_) => {
            let fetched = match email::read_next_email().await? {
                Some(email) => email,
                None => {
                    return Ok(Vec::new());
                }
            };

            let chain = host::config_var("CHAIN")
                .ok_or_else(|| anyhow::anyhow!("CHAIN config var is required"))?;
            let handler =
                host::config_var("SERVICE_HANDLER_CONTRACT_ADDRESS").ok_or_else(|| {
                    anyhow::anyhow!("SERVICE_HANDLER_CONTRACT_ADDRESS config var is required")
                })?;
            let backend = app_component_backend_client::BackendClient::from_env()?;
            let prepared = async {
                // Expired retries use the same confirmed-rejection path below.
                // Do not execute an instruction that exceeded its retry window.
                if fetched
                    .cache
                    .verification_expired(fetched.id)
                    .map_err(error::AppError::EmailCache)?
                {
                    return Err(email::parser::InvalidEmail::VerificationExpired.into());
                }
                let email = fetched
                    .message
                    .as_ref()
                    .map_err(|reason| error::AppError::InvalidEmail(*reason))?;
                email.sender_domain()?;
                email.validate_subject()?;
                let salt = email.event_id_salt()?;
                verify_email(email).await?;
                Ok::<_, error::AppError>((email, salt))
            }
            .await;
            let (email, event_id_salt) = match prepared {
                Ok(prepared) => prepared,
                Err(error::AppError::InvalidEmail(reason)) => {
                    backend::reject(
                        &backend,
                        &chain,
                        &handler,
                        fetched.id,
                        reason,
                        &fetched.cache,
                    )
                    .await?;
                    return Ok(Vec::new());
                }
                Err(error::AppError::VerificationUnavailable) => {
                    fetched
                        .cache
                        .defer(fetched.id)
                        .map_err(error::AppError::EmailCache)?;
                    eprintln!("email_verification_unavailable: retry scheduled");
                    return Ok(Vec::new());
                }
                Err(error) => return Err(error.into()),
            };
            let payload = CustomExecuteMsg::Email(UserIdEmail {
                from: UserId::new_email_address(&email.original_sender),
                subject: email.subject.clone().unwrap_or_default(),
            });
            let submit = backend::register(
                &backend,
                &chain,
                &handler,
                &host::get_event_id(Some(&event_id_salt)),
                &payload,
                fetched.id,
            )
            .await?;
            if !submit {
                fetched.remember()?;
                return Ok(Vec::new());
            }
            let response = WasmResponse {
                payload: payload.encode().map_err(|e| anyhow::anyhow!("{e:?}"))?,
                ordering: None,
                event_id_salt: Some(event_id_salt),
            };
            fetched.remember()?;
            return Ok(vec![response]);
        }
        TriggerData::Raw(data) => {
            let data = std::str::from_utf8(&data)?;
            match data {
                "read-mail" => {
                    let email = match email::read_next_email().await? {
                        Some(email) => email,
                        None => {
                            println!("No new email found.");
                            return Ok(Vec::new());
                        }
                    };

                    let email = email.message?;
                    println!("{:#?}", email);

                    let verification_result = verify_email(&email).await;
                    match verification_result {
                        Ok(_) => println!("Email verification succeeded."),
                        Err(e) => println!("Email verification failed: {:?}", e),
                    }
                }
                _ => {
                    bail!("Unknown command: {}", data);
                }
            }
        }
        _ => {
            bail!("Unsupported TriggerData variant");
        }
    }
    Ok(Vec::new())
}

export!(Component);
