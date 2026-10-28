# Upstream provenance

Copied from https://github.com/Lay3rLabs/cw-middleware at tag
`v0.2.0-alpha.15`:

- `packages/contracts/mirror/stake-registry/src/{entry,state,error}.rs`
- `packages/contracts/mirror/api/src/stake_registry.rs` (now in our contract API)

Copyright attribution: Layer Labs (Cayman), upstream workspace author.
Upstream declares `GPL-3.0-or-later` in its workspace Cargo.toml; this copy retains
that license. The pinned upstream repository does not contain a separate license
file at its root. License text is included in `LICENSE`.

The upstream repository is no longer maintained according to the project owner.
We maintain this copy so deployment can explicitly assign operator-management
authority without requiring the committee to instantiate the contract itself.

Local changes: accept a required, validated initial `owner` instead of assigning
`info.sender`; add an owner-only, validated, one-step `TransferOwnership` message
and an `Owner` query; use local API/package imports and workspace dependencies.
Existing operator execute/query messages, signature checks, and weight snapshots
remain unchanged. No migration entry point is added. The registry still creates
the local service-manager copy with Instantiate2, preserving the factory link.

Deployment initially assigns the CLI wallet ownership of both middleware contracts.
It configures the full operator set, then atomically transfers registry ownership
and manager administration to the Hydro committee. This removes the need for any
DAO operator-registration transaction during bootstrap. The CLI loses ownership
immediately on transfer. Review relevant upstream/security changes explicitly if
adopting future updates.
