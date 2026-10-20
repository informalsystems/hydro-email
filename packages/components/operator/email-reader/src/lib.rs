#![allow(warnings)]
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

            let email = &fetched.message;
            verify_email(email).await?;

            let event_id_salt = email.event_id_salt()?;

            let user_id = UserId::new_email_address(&email.original_sender);

            let email = UserIdEmail {
                from: user_id,
                subject: email.subject.clone().unwrap_or_default(),
            };

            println!("Got email: {:#?}", email);
            println!("Proxy execute msg: {:#?}", email.proxy_execute_msg());
            println!("Event ID salt: {}", const_hex::encode(&event_id_salt));

            let response = WasmResponse {
                payload: CustomExecuteMsg::Email(email)
                    .encode()
                    .map_err(|e| anyhow::anyhow!("{e:?}"))?,
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

                    let email = email.message;
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
