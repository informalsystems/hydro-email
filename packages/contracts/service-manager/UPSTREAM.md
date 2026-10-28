# Upstream provenance

Copied from https://github.com/Lay3rLabs/cw-middleware at tag
`v0.2.0-alpha.15`:

- `packages/contracts/mirror/service-manager/src/{entry,state}.rs`
- `packages/contracts/mirror/api/src/service_manager.rs` (now in our contract API)

Copyright attribution: Layer Labs (Cayman), upstream workspace author.
Upstream declares `GPL-3.0-or-later`; license text is included in `LICENSE`.
The project owner reports that upstream is no longer maintained.

Local changes: add an admin-only, validated, one-step `TransferAdmin` message;
initialize the service URI to an empty string until configured; use local
package/API imports and workspace dependencies. Existing WAVS query/execute wire
formats, signature validation, and quorum calculation remain unchanged. No
migration entry point is added. Administration is not upgrade authority.

Both middleware contracts initially belong to the CLI wallet. It configures
URI/quorum and registers the complete operator set, then atomically transfers
manager administration and stake-registry ownership to the committee. No DAO
acceptance or setup transactions are needed. The former admin cannot make later
changes after transfer. Review relevant upstream/security changes explicitly if
adopting future updates.
