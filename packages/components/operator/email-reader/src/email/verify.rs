use std::sync::Arc;

use sloggers::{null::NullLoggerBuilder, Build};

use crate::{email::parser::EmailMessage, error::AppResult};

pub async fn verify_email(email: &EmailMessage) -> AppResult<()> {
    let resolver = cfdkim::dns::wasi::WasiCloudflareLookup {};

    let from_domain = email.sender_domain()?;

    cfdkim::verify_email_with_resolver(
        &NullLoggerBuilder.build()?,
        &from_domain,
        &email.get_parsed()?,
        Arc::new(resolver),
    )
    .await?;

    Ok(())
}
