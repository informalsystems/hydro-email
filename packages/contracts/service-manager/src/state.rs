// SPDX-License-Identifier: GPL-3.0-or-later
// Derived from Lay3rLabs/cw-middleware v0.2.0-alpha.15, Layer Labs (Cayman).
// See ../UPSTREAM.md for provenance and local changes.

use cosmwasm_std::{Addr, Uint256};
use cw_storage_plus::Item;

pub const ADMIN: Item<Addr> = Item::new("admin");
pub const SERVICE_URI: Item<String> = Item::new("service-uri");
pub const STAKE_REGISTRY: Item<Addr> = Item::new("stake-registry");

// Quorum configuration
pub const QUORUM_NUMERATOR: Item<Uint256> = Item::new("quorum_numerator");
pub const QUORUM_DENOMINATOR: Item<Uint256> = Item::new("quorum_denominator");
