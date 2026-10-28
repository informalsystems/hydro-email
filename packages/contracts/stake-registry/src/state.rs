// SPDX-License-Identifier: GPL-3.0-or-later
// Derived from Lay3rLabs/cw-middleware v0.2.0-alpha.15, Layer Labs (Cayman).
// See ../UPSTREAM.md for provenance and local changes.

use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Addr, Uint256};
use cw_storage_plus::{Item, Map, SnapshotMap, Strategy};
use layer_climb_address::EvmAddr;

// Contract configuration
pub const CONFIG: Item<Config> = Item::new("config");
pub const OWNER: Item<Addr> = Item::new("owner");

// Operator mappings with historical tracking
pub const OPERATOR_WEIGHTS: SnapshotMap<String, Uint256> = SnapshotMap::new(
    "operator_weights",
    "operator_weights__checkpoints",
    "operator_weights__changelog",
    Strategy::EveryBlock,
);
pub const OPERATOR_TO_SIGNING_KEY: SnapshotMap<String, EvmAddr> = SnapshotMap::new(
    "operator_to_signing_key",
    "operator_to_signing_key__checkpoints",
    "operator_to_signing_key__changelog",
    Strategy::EveryBlock,
);
pub const SIGNING_KEY_TO_OPERATOR: SnapshotMap<String, EvmAddr> = SnapshotMap::new(
    "signing_key_to_operator",
    "signing_key_to_operator__checkpoints",
    "signing_key_to_operator__changelog",
    Strategy::EveryBlock,
);
pub const OPERATOR_REGISTERED: Map<String, bool> = Map::new("operator_registered");

// Weight tracking
pub const TOTAL_WEIGHT: Item<Uint256> = Item::new("total_weight");

#[cw_serde]
pub struct Config {
    pub service_manager: Addr,
}

impl Config {
    pub fn new(service_manager: Addr) -> Self {
        Self { service_manager }
    }
}
