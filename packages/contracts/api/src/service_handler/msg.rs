use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::{Addr, HexBinary};
use wavs_types::contracts::cosmwasm::service_handler::{
    ServiceHandlerExecuteMessages, ServiceHandlerQueryMessages,
};

use crate::{proxy::ProxyExecuteMsg, user_registry::msg::UserId};

#[cw_serde]
pub struct InstantiateMsg {
    pub auth: Auth,
    /// The UserRegistry contract address
    pub user_registry: String,
}

#[cw_serde]
pub enum Auth {
    /// Implement ServiceHandler interface, validate signatures with the ServiceManager
    ServiceManager(String),
    /// Used for tests. One account is authorized to execute the privileged methods normally reserved for WAVS
    Admin(String),
}

#[cw_serde]
#[schemaifier(mute_warnings)]
#[derive(QueryResponses)]
#[query_responses(nested)]
#[serde(untagged)]
pub enum QueryMsg {
    Custom(CustomQueryMsg),
    Wavs(ServiceHandlerQueryMessages),
}

#[cw_serde]
#[derive(QueryResponses)]
pub enum CustomQueryMsg {
    /// Whether a 20-byte WAVS event ID has been successfully processed.
    #[returns(EventProcessedResponse)]
    EventProcessed { event_id: HexBinary },

    #[returns(EmailUserIdsResponse)]
    EmailUserIds {
        /// Maximum number of user IDs to return (default 30, capped at 100).
        limit: Option<u32>,
        /// Optional exclusive start of the range (last key from previous page)
        start_after: Option<UserId>,
    },
    #[returns(EmailsFromResponse)]
    EmailsFrom {
        /// Sender's user ID.
        from: String,
        /// Maximum number of emails to return (default 30, capped at 100).
        limit: Option<u32>,
        /// Optional exclusive start of the range (last key from previous page)
        start_after: Option<u64>,
    },
    #[returns(EmailsResponse)]
    Emails {
        /// Maximum number of emails to return (default 30, capped at 100).
        limit: Option<u32>,
        /// Optional exclusive start of the range (last key from previous page)
        start_after: Option<u64>,
    },
    #[returns(AdminResponse)]
    Admin {},

    #[returns(UserRegistryResponse)]
    UserRegistry {},
}

#[cw_serde]
pub struct EmailsFromResponse {
    /// List of (email, pagination key)
    pub emails: Vec<(EmailMessageOnly, u64)>,
}

#[cw_serde]
pub struct EventProcessedResponse {
    pub processed: bool,
}

#[cw_serde]
pub struct EmailUserIdsResponse {
    /// List of email addresses
    pub email_user_ids: Vec<UserId>,
}

#[cw_serde]
pub struct EmailsResponse {
    /// List of (email, pagination key)
    pub emails: Vec<(UserIdEmail, u64)>,
}

#[cw_serde]
pub struct EmailMessageOnly {
    pub subject: String,
}

impl From<&UserIdEmail> for EmailMessageOnly {
    fn from(src: &UserIdEmail) -> Self {
        Self {
            subject: src.subject.clone(),
        }
    }
}

#[cw_serde]
pub struct UserIdEmail {
    pub from: UserId,
    pub subject: String,
}

impl UserIdEmail {
    pub fn proxy_execute_msg(&self) -> cosmwasm_std::StdResult<ProxyExecuteMsg> {
        ProxyExecuteMsg::from_email_subject(&self.subject)
    }
}

#[cw_serde]
#[schemaifier(mute_warnings)]
#[serde(untagged)]
pub enum ExecuteMsg {
    Custom(CustomExecuteMsg),
    Wavs(ServiceHandlerExecuteMessages),
}

#[cw_serde]
pub enum CustomExecuteMsg {
    /// Got an email
    Email(UserIdEmail),
}

impl CustomExecuteMsg {
    pub fn encode(&self) -> cosmwasm_std::StdResult<Vec<u8>> {
        cosmwasm_std::to_json_vec(self)
    }

    pub fn decode(bytes: impl AsRef<[u8]>) -> cosmwasm_std::StdResult<Self> {
        cosmwasm_std::from_json(bytes)
    }
}

#[cw_serde]
pub struct AdminResponse {
    pub admin: Option<String>,
}

#[cw_serde]
pub struct UserRegistryResponse {
    pub address: Addr,
}

#[cw_serde]
pub struct MigrateMsg {}
