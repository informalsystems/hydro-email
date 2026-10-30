use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::Addr;
use cw_storage_plus::NewTypeKey;
use sha2::{Digest, Sha256};

#[cw_serde]
pub struct InstantiateMsg {
    /// Cold authority that configures the factory and rotates provisioning admins.
    pub owner: String,
    pub admins: Vec<String>,
}

#[cw_serde]
#[serde(deny_unknown_fields)]
#[schemaifier(mute_warnings)]
pub enum ExecuteMsg {
    /// Create this user's deterministic proxy and register it atomically.
    RegisterUser {
        user_id: UserId,
    },
    /// Owner-only, one-time setup after the service handler has been deployed.
    ConfigureProxy {
        proxy_code_id: u64,
        service_handler: String,
        control_center_registry: String,
    },
    UpdateAdmins {
        admins: Vec<String>,
    },
    TransferOwnership {
        owner: String,
    },
}

#[cw_serde]
#[derive(QueryResponses)]
pub enum QueryMsg {
    #[returns(ProxyAddressResponse)]
    ProxyAddress { user_id: UserId },
    #[returns(ConfigResponse)]
    Config {},
}

#[cw_serde]
pub struct ProxyConfig {
    pub proxy_code_id: u64,
    pub service_handler: Addr,
    pub control_center_registry: Addr,
}

#[cw_serde]
pub struct ConfigResponse {
    pub owner: Addr,
    pub admins: Vec<Addr>,
    pub proxy: Option<ProxyConfig>,
}

#[cw_serde]
pub struct ProxyAddressResponse {
    pub address: Addr,
}

#[cw_serde]
#[derive(NewTypeKey)]
pub struct UserId(String);

impl std::fmt::Display for UserId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl UserId {
    pub const SALT: [u8; 12] = *b"hydro-email!";

    pub fn new_raw(user_id: String) -> Self {
        Self(user_id)
    }

    /// Derive an ID from the extracted mailbox, lowercasing only the domain.
    /// Local-part case is preserved so distinct mailboxes cannot share an identity.
    pub fn new_email_address(email: &str) -> Self {
        let email = match extract_email(email) {
            Some(e) => e,
            None => email.to_string(),
        };

        let mut hasher = Sha256::new();
        match email.rsplit_once('@') {
            Some((local, domain)) => {
                hasher.update(local);
                hasher.update("@");
                hasher.update(domain.to_ascii_lowercase());
            }
            None => hasher.update(&email),
        }
        hasher.update(Self::SALT);

        Self(const_hex::encode(hasher.finalize()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// One canonical salt per SHA-256 user ID; reject uppercase hex aliases.
    pub fn proxy_salt(&self) -> cosmwasm_std::StdResult<Vec<u8>> {
        if self.0.len() != 64
            || !self
                .0
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(cosmwasm_std::StdError::msg(
                "Expected a lowercase SHA-256 user ID",
            ));
        }
        const_hex::decode(&self.0).map_err(cosmwasm_std::StdError::msg)
    }
}

fn extract_email(input: &str) -> Option<String> {
    let parsed = mailparse::addrparse(input).ok()?;
    // addrparse returns a vector; usually you want the first entry
    let addr = parsed.first()?;

    match addr {
        mailparse::MailAddr::Single(single) => Some(single.addr.clone()),
        mailparse::MailAddr::Group(group) => {
            // Rare, but groups exist in RFC 5322
            group.addrs.first().map(|a| a.addr.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_id_normalizes_domain_case_and_preserves_local_part_case() {
        let expected = UserId::new_email_address("Alice@example.com");
        for email in [
            "Alice@EXAMPLE.COM",
            "Alice@ExAmPlE.CoM",
            "Alice <Alice@EXAMPLE.COM>",
        ] {
            assert_eq!(UserId::new_email_address(email), expected);
        }
        assert_ne!(UserId::new_email_address("alice@example.com"), expected);
        assert_ne!(UserId::new_email_address("ALICE@example.com"), expected);
        assert_eq!(
            UserId::new_email_address("alice@example.com").as_str(),
            "7287a82555fca2e66e49eca0607a4007881f661e3588b408a71f5870773ed57c"
        );
    }
}
