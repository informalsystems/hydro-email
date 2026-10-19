use cosmwasm_std::{CheckedFromRatioError, DecimalRangeExceeded, OverflowError, StdError};
use cw_utils::PaymentError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("{0}")]
    Payment(#[from] PaymentError),

    #[error("{0}")]
    Overflow(#[from] OverflowError),

    #[error("{0}")]
    DecimalRangeExceeded(#[from] DecimalRangeExceeded),

    #[error("{0}")]
    CheckedFromRatioError(#[from] CheckedFromRatioError),

    #[error("Unauthorized")]
    Unauthorized,

    #[error("Event already processed")]
    EventAlreadyProcessed,

    #[error("Unknown reply id: {id}")]
    UnknownReplyId { id: u64 },

    #[error("ABI decode: {0}")]
    AbiDecode(String),

    #[error("Payload decode: {0}")]
    PayloadDecode(String),
}
