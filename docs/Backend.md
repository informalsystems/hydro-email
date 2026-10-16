# Shared backend

`packages/backend` is a native Rust service using Axum and Postgres. Its first
feature is email-to-proxy registration. It shares the existing contract API,
email ID derivation, and chain clients. It runs independently of WAVS.

## Local setup

Add the `BACKEND_*` settings from `.env.example` to your existing `.env` without
overwriting your other configuration. Set a random registration API credential
of at least 32 characters and the chain key and deployed registry address.
`BACKEND_DATABASE_URL` must match the local database password and configured port.
Shared defaults belong in `taskfile/config.yml`; secrets and deployment-specific
values belong in `.env`. The service also accepts environment variables directly.

```sh
task backend:postgres-start
task backend:api
```

The database has a persistent Docker volume and is bound only to localhost.
`task backend:postgres-stop` preserves the volume. Existing `backend:start-all`
and WAVS stop commands do not start, stop, or remove this database.

For a clean local database, stop the API and run:

```sh
task backend:postgres-stop RESET=true
task backend:postgres-start
task backend:api
```

`RESET=true` removes the local database container and its data volume, deleting
all local records. The next start creates an empty database and the API applies
the initial migration. This does not affect a hosted database supplied through
`BACKEND_DATABASE_URL` or reset any on-chain contracts. Reset an earlier development
database before running this simplified initial schema.

Migrations embedded in the binary run at startup. `/healthz` is liveness;
`/readyz` verifies database connectivity. Both are unauthenticated and disclose no
configuration.

Provisioning is **off by default**. In this mode authenticated requests are saved
as queued jobs, with no chain connections or transactions. To enable it, configure:

- `BACKEND_PROXY_CODE_ID`: existing uploaded Hydro proxy code ID.
- `BACKEND_SERVICE_HANDLER_ADDRESS`: proxy application admin.
- `BACKEND_CONTROL_CENTER_ADDRESSES`: space-separated configured control centers.
- `BACKEND_SIGNING_MNEMONIC`: a funded signer authorized by the registry.
- `BACKEND_PROVISIONING_ENABLED=true`.

Enabling provisioning also processes existing queued jobs. Check the database
queue before starting it against a live chain; these jobs can spend gas.

The current registry sets its admins at instantiation and exposes no admin update
method. A new wallet is not automatically authorized. The worker checks contracts
exist and simulates a registration at startup to verify permission before spending
gas on a proxy. Simulation does not broadcast or write chain state.

The signer must be dedicated to this provisioner. In particular, do not share it
with WAVS aggregators or a CLI concurrently. Keep deployment configuration stable
while jobs are in progress. One provisioner runs per chain, enforced by a dedicated
Postgres session advisory lock; a second provisioner exits rather than running
concurrently. API-only instances can run separately with provisioning disabled.

## API

All `/v1/registrations` routes require an API key via
`Authorization: Bearer <API key>`, matching `BACKEND_REGISTRATION_API_KEY`.
Health endpoints do not require authentication. The backend enforces global
registration quotas; per-IP throttling is not implemented.

### Submit registration

`POST /v1/registrations` with JSON:

```json
{ "email": "alice@example.com" }
```

Only a single plain ASCII mailbox is accepted, without a display name. Surrounding
whitespace is trimmed. Case is preserved to match the existing operator's email ID
derivation; case variants are not collapsed. There is no email verification.
The caller cannot choose a proxy, admin, control center, or chain.

- `202`: new registration or explicitly requested retry queued.
- `200`: existing active or completed registration returned.
- `400`: invalid email.
- `401`: invalid/missing credential.
- `413`: request exceeds 4 KiB.
- `422`: invalid JSON shape, including unknown fields.
- `429`: registration quota exhausted.
- `503`: database unavailable; no new job is accepted.

### Read status

`GET /v1/registrations/{user_id}` returns the same registration representation.
This route also requires the API key. User IDs are deterministic hashes,
not secrets or proof of email ownership.

```json
{
  "user_id": "<64 hexadecimal characters>",
  "status": "completed",
  "attempt_number": 1,
  "proxy_address": "<chain address>",
  "create_tx_hash": "<hash>",
  "register_tx_hash": "<hash>",
  "error_code": null,
  "created_at": "<UTC timestamp>",
  "updated_at": "<UTC timestamp>"
}
```

Email addresses are stored in Postgres but excluded from responses. Internal RPC
errors and credentials are not returned. Access to Postgres includes access to
email addresses; restrict it accordingly.

## Provisioning and failure behavior

```text
queued → creating_proxy → proxy_pending → proxy_created
       → registering → registration_pending → completed
```

The worker first queries the registry. If a mapping already exists, it imports
that mapping without creating a proxy. Otherwise it uses `instantiate2` with the
32-byte email ID as the salt. The creator wallet, code checksum, and salt determine
the proxy address. An existing proxy at that address is validated and reused.
The worker then registers the mapping in a separate transaction and verifies it
before reporting completion. The initial deployment's configured example email
can remain a dummy account; it uses the existing deployment helper.

Intent and a unique attempt record are committed before each broadcast. A CheckTx hash
means pending inclusion, not success. The worker polls for confirmation without
rebroadcasting. Confirmed chain failure or explicit rejection is `failed`.
Network errors or interrupted attempts without a saved hash become `unknown`.
The expected proxy address is saved before creation, so it may appear in a response
before the contract exists. Creation can succeed while registration fails; the
worker keeps that address and reuses the proxy when registration is resumed.

Posting the same email returns an active or completed registration unchanged.
For a `failed` or `unknown` registration, it queues an explicit retry and increments
`attempt_number`. Concurrent retry requests queue only one attempt. The worker
checks the mapping and deterministic proxy address again before submitting.
There are no automatic submission retries. A late original transaction may still
land, but instantiation at the same address and duplicate registry mappings are
rejected on-chain; an explicit retry can therefore still incur additional fees.

Postgres and the chain are not atomic. A crash between broadcast and saving its
hash can leave an unknown outcome. Known hashes continue to be checked. A mapping
query can resolve an uncertain registration. The expected proxy can also be queried
directly even if the creation transaction hash was lost. Pending and
unknown attempts block further provisioning in the same registry to avoid using
the wallet while its sequence may be uncertain. Explicit retries resume work;
pending transactions are checked without starting parallel attempts.

`registration_transitions` keeps the status history for each attempt.
`registration_attempts` keeps individual submission records and transaction hashes.

The worker sleeps one second between steps. Deferred jobs become eligible for
another check after five seconds. Completion time also depends on block inclusion,
RPC latency, and queued work.

Run with `RUST_LOG=info task backend:api` to log transaction submissions and
confirmed registrations, including user IDs and transaction hashes or proxy
addresses. Broadcast errors include their available details in server logs;
database and API error codes remain stable.

## Quotas and gas

New jobs and explicit retries reserve capacity under an atomic Postgres admission lock. Registration
quotas are rolling one-hour and 24-hour windows across registries for the configured
chain key. Failed and unknown registrations still consume admission capacity.
Active or completed duplicates return their existing record even when the quota is full.

Transactions use the chain client's simulation-based gas estimation and the
selected chain's configured gas price and fee denomination. There are no separate
per-transaction or daily backend gas settings. Registration quotas limit accepted
requests, not the timing or monetary cost of transactions: older queued jobs can
be processed later. Each new registration normally needs two transactions.

`registration_attempts` permits one submission per stage per explicitly requested
attempt, preventing parallel submissions while allowing registration recovery.

## Validation

To run the full backend suite, including Postgres integration tests:

1. Set `BACKEND_DATABASE_URL` in `.env` to your local or dedicated test Postgres server.
2. Ensure Postgres is running. For the local Docker database, run
   `task backend:postgres-start`. Skip this step if your hosted test database is
   already running.
3. Run:

```sh
task backend:api-test
```

**You do not need to run `task backend:api` for these tests.** The tests exercise
the HTTP handlers and worker directly, using a fake chain. They require no
deployed contracts, signing wallet, API credential, or running WAVS instance.
They do not broadcast chain transactions.

These tests cover concurrent admission, quotas, submission-attempt uniqueness, HTTP access,
successful provisioning with a fake chain, and uncertain/failed submissions.

For unit tests alone, no database is needed. Plain `cargo test` skips the Postgres
integration tests. Additional formatting/build checks can also run independently:

```sh
cargo test -p app-backend
cargo clippy -p app-backend --all-targets -- -D warnings
cargo check -p app-backend -p off-chain-tests --all-targets
```

### Live-chain test

Local Postgres and a local API can use an existing deployment on a remote chain.
This test spends real gas when provisioning is enabled.

1. Configure the chain, registry, proxy code ID, service handler, control centers,
   and funded registry-authorized signer as described above. Deployment records
   are in `.deployments/` or do a fresh deployment if needed. Check existing queued
   jobs before enabling provisioning.
2. Start the database, set `BACKEND_PROVISIONING_ENABLED=true`, and run
   `RUST_LOG=info task backend:api`. Verify `curl -i http://127.0.0.1:8080/readyz`
   returns `200`.
3. In another Bash terminal, enter the configured API key and submit a fresh test
   email. Expect `202` and save the returned `user_id`:

   ```sh
   read -rsp "Backend API key: " HYDRO_API_KEY
   curl -i -sS http://127.0.0.1:8080/v1/registrations \
     -H "Authorization: Bearer $HYDRO_API_KEY" \
     -H "Content-Type: application/json" \
     -d '{"email":"backend-smoke-test@example.com"}'
   ```

4. Query status until `completed`, using the returned ID:

   ```sh
   curl -sS 'http://127.0.0.1:8080/v1/registrations/<user_id>' \
     -H "Authorization: Bearer $HYDRO_API_KEY"
   ```

   Verify the returned proxy's configuration and query the registry's
   `{"proxy_address":{"user_id":"<user_id>"}}` mapping independently. For Cosmos
   Hub, use `gaiad query wasm contract-state smart <registry_address> '<query>'
--node <rpc_endpoint> --output json`. The mapping must match `proxy_address`.
   Postgres should contain one completed registration, its transition history,
   and two submission records with the returned transaction hashes.

5. Repeat the same POST. Expect `200`, still completed, with the same attempt
   number, proxy, and hashes, and no new submission records. Stop the API and set
   provisioning back to `false` when finished.
