//! Local proxy contract message types and utilities.
//!
//! These types are duplicated from `hydro_proxy::msg` rather than imported because
//! the hydro submodule uses cosmwasm_std 2.x while this crate uses cosmwasm_std 3.x.
//! The Rust type system treats types from different crate versions as incompatible,
//! even if they have identical definitions. Since the JSON serialization format is
//! the same between versions, we can safely duplicate the types here and they will
//! serialize correctly when sent to the proxy contract.

use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Coin, StdError, StdResult, Uint128};

/// Execute messages for the proxy contract.
/// Mirrors `hydro_proxy::msg::ExecuteMsg`.
#[cw_serde]
pub enum ProxyExecuteMsg {
    ForwardToInflow {},
    WithdrawReceiptTokens { address: String, coin: Coin },
    WithdrawFunds { address: String, coin: Coin },
}

impl ProxyExecuteMsg {
    /// Convert to email subject string.
    /// - ForwardToInflow -> "deposit"
    /// - WithdrawFunds -> "withdraw ADDRESS DENOM AMOUNT"
    /// - WithdrawReceiptTokens -> "withdraw_receipt ADDRESS DENOM AMOUNT"
    pub fn to_email_subject(&self) -> String {
        match self {
            ProxyExecuteMsg::ForwardToInflow {} => "deposit".to_string(),
            ProxyExecuteMsg::WithdrawFunds { address, coin } => {
                format!("withdraw {} {} {}", address, coin.denom, coin.amount)
            }
            ProxyExecuteMsg::WithdrawReceiptTokens { address, coin } => {
                format!(
                    "withdraw_receipt {} {} {}",
                    address, coin.denom, coin.amount
                )
            }
        }
    }

    /// Parse one supported instruction. Command names are case-insensitive;
    /// address and denom case are preserved and their validation is left to execution.
    ///
    /// Formats:
    /// - "forward" or "deposit"
    /// - "withdraw ADDRESS DENOM AMOUNT"
    /// - "withdraw_receipt ADDRESS DENOM AMOUNT"
    ///
    /// Withdrawals require a positive decimal integer amount fitting Uint128.
    /// Empty subjects, unknown commands, and missing/extra arguments are errors.
    pub fn from_email_subject(subject: &str) -> StdResult<Self> {
        let parts: Vec<_> = subject.split_whitespace().collect();
        let invalid = || StdError::msg("Invalid email instruction");
        let Some(command) = parts.first() else {
            return Err(invalid());
        };
        match command.to_ascii_lowercase().as_str() {
            "forward" | "deposit" if parts.len() == 1 => Ok(Self::ForwardToInflow {}),
            "withdraw" | "withdraw_receipt" if parts.len() == 4 => {
                if !parts[3].bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(StdError::msg("Invalid withdrawal amount"));
                }
                let amount: Uint128 = parts[3]
                    .parse()
                    .map_err(|_| StdError::msg("Invalid withdrawal amount"))?;
                if amount.is_zero() {
                    return Err(StdError::msg("Withdrawal amount must be positive"));
                }
                let address = parts[1].to_owned();
                let coin = Coin {
                    denom: parts[2].to_owned(),
                    amount: amount.into(),
                };
                Ok(if command.eq_ignore_ascii_case("withdraw_receipt") {
                    Self::WithdrawReceiptTokens { address, coin }
                } else {
                    Self::WithdrawFunds { address, coin }
                })
            }
            _ => Err(invalid()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_supported_commands_with_case_and_whitespace_variations() {
        for subject in ["deposit", "FORWARD", "  DePoSiT "] {
            assert_eq!(
                ProxyExecuteMsg::from_email_subject(subject).unwrap(),
                ProxyExecuteMsg::ForwardToInflow {}
            );
        }
        assert_eq!(
            ProxyExecuteMsg::from_email_subject("  WiThDrAw\tAddressUnchanged  ibc/ABC123  42 ")
                .unwrap(),
            ProxyExecuteMsg::WithdrawFunds {
                address: "AddressUnchanged".into(),
                coin: Coin::new(42u128, "ibc/ABC123")
            },
        );
        assert_eq!(
            ProxyExecuteMsg::from_email_subject("WITHDRAW_RECEIPT address factory/vault/Share 1")
                .unwrap(),
            ProxyExecuteMsg::WithdrawReceiptTokens {
                address: "address".into(),
                coin: Coin::new(1u128, "factory/vault/Share")
            },
        );
    }

    #[test]
    fn rejects_empty_unsupported_and_malformed_instructions() {
        for subject in [
            "",
            "   ",
            "random subject",
            "deposit extra",
            "forward extra",
            "withdraw",
            "withdraw address denom",
            "withdraw address denom 1 extra",
            "withdraw_receipt address denom",
            "withdraw_receipt address denom 1 extra",
            "withdraw address denom zero",
            "withdraw address denom 0",
            "withdraw address denom -1",
            "withdraw address denom +1",
            "withdraw address denom 1.5",
            "withdraw address denom 340282366920938463463374607431768211456",
            "withdraw_receipt address denom 0",
        ] {
            assert!(
                ProxyExecuteMsg::from_email_subject(subject).is_err(),
                "Accepted {subject:?}"
            );
        }
    }

    #[test]
    fn supported_operations_round_trip_without_changing_arguments() {
        for message in [
            ProxyExecuteMsg::ForwardToInflow {},
            ProxyExecuteMsg::WithdrawFunds {
                address: "address".into(),
                coin: Coin::new(Uint128::MAX, "ibc/ABC123"),
            },
            ProxyExecuteMsg::WithdrawReceiptTokens {
                address: "address".into(),
                coin: Coin::new(500u128, "factory/vault/Share"),
            },
        ] {
            assert_eq!(
                ProxyExecuteMsg::from_email_subject(&message.to_email_subject()).unwrap(),
                message
            );
        }
    }
}
