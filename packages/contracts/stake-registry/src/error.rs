// SPDX-License-Identifier: GPL-3.0-or-later
// Derived from Lay3rLabs/cw-middleware v0.2.0-alpha.15, Layer Labs (Cayman).
// See ../UPSTREAM.md for provenance and local changes.

use cosmwasm_std::{Instantiate2AddressError, StdError};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("{0}")]
    Instantiate2Address(#[from] Instantiate2AddressError),

    #[error("Unauthorized")]
    Unauthorized {},

    #[error("Registration not supported")]
    RegistrationNotSupported {},

    #[error("Deregistration not supported")]
    DeregistrationNotSupported {},

    #[error("Signing key update not supported")]
    SigningKeyUpdateNotSupported {},

    #[error("Operator update not supported")]
    OperatorUpdateNotSupported {},

    #[error("Invalid array lengths")]
    InvalidArrayLengths {},

    #[error("Invalid signature")]
    InvalidSignature {},

    #[error("Operator not registered")]
    OperatorNotRegistered {},

    #[error("Insufficient voting power")]
    InsufficientVotingPower {},

    #[error("Invalid signature data format")]
    InvalidSignatureDataFormat {},
}
