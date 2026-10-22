# Shared backend

`packages/backend` is a native Rust service using Axum and Postgres. Its first
feature is email-to-proxy registration. It shares the existing contract API,
email ID derivation, and chain clients. It runs independently of WAVS.

## Local setup

Add the `BACKEND_*` settings from `.env.example` to your existing `.env` without
overwriting your other configuration. Set a random backend API credential
of at least 32 characters, the chain key, and deployed registry and service-handler
addresses.
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
all migrations. This does not affect a hosted database supplied through
`BACKEND_DATABASE_URL` or reset any on-chain contracts. Reset an earlier development
database before running this simplified initial schema.

Migrations embedded in the binary run at startup. `/healthz` is liveness;
`/readyz` verifies database connectivity. Both are unauthenticated and disclose no
configuration.

Provisioning is **off by default**. Registration requests are saved as queued
jobs without broadcasting transactions. Action reconciliation still performs
read-only chain queries. To enable registration provisioning, configure:

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

## Registration API

All `/v1/registrations` routes require an API key via
`Authorization: Bearer <API key>`, matching `BACKEND_API_KEY`.
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

## Registration provisioning and failures

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

## Action submission storage

Migration `0002_actions.sql` adds storage for WAVS actions:

- `actions`: decoded payload, user ID, current outcome, transaction hash, and
  reconciliation timestamps. Events are unique per chain, service-handler address,
  and 20-byte event ID stored as lowercase hex.
- `action_transitions`: history of outcome changes for each action.
- `action_mailbox_messages`: mailbox identifiers and acknowledgment state, separate
  from execution. An action can have multiple messages, such as duplicate copies.

Actions start as `received`, then become `submitting` while WAVS handles submission
and confirmation. Outcomes are `completed`, `failed`, or `outcome_unknown`.
Creating an action records its initial `received` transition atomically. Result
reporting updates the action and history atomically. The WAVS aggregator reports
actions and callback results through these endpoints. The reconciliation worker
checks unfinished records against the chain. Email validation failures before an
action exists remain separate work.

### Create an action

`POST /v1/actions` uses the same `Authorization: Bearer <API key>` authentication
as registration. This is a server-to-server endpoint.

```json
{
  "event_id": "<40 lowercase hexadecimal characters>",
  "payload": {
    "email": {
      "from": "<64 lowercase hexadecimal user ID>",
      "subject": "deposit"
    }
  }
}
```

`payload` uses the service handler's `CustomExecuteMsg` format. Its `from` field
already contains the hashed user ID; the backend stores that value without hashing
it again. The backend scopes the action to `BACKEND_CHAIN_KEY` and
`BACKEND_SERVICE_HANDLER_ADDRESS`, never caller-supplied deployment settings.
Action creation is available with provisioning disabled and does not submit transactions.
The handler address must be configured.

`POST /v1/actions` also accepts an optional `mailbox_message` alongside `event_id`
and `payload`:

```json
{
  "mailbox_message": {
    "folder": "INBOX",
    "uid_validity": 100,
    "uid": 10
  }
}
```

These fields identify an email in the configured receiving account so the backend
can later mark it read. Each mailbox message belongs to one action. An action can
have multiple messages, such as duplicate deliveries that each need acknowledgment.
Repeating the same metadata does not create another record or reset acknowledgment.

The reader supplies this field. Aggregator callbacks may omit it.

- `201`: action and initial history entry created with status `received`.
- `200`: same event and payload already exist; current record returned unchanged.
- `400`: invalid event, user ID, or mailbox identifiers.
- `401`: missing or invalid API credential.
- `409`: event exists with different sender or payload, or the mailbox message
  already belongs to another action.
- `413`: request exceeds 4 KiB.
- `422`: malformed request shape, unsupported payload, or unknown top-level field.
- `503`: database unavailable.

The response contains `id`, `chain_key`, `service_handler_address`, `event_id`,
`user_id`, `payload`, `status`, `tx_hash`, `error_code`, `error_detail`, `submitted_at`,
`closed_at`, `last_checked_at`, `next_check_at`, `created_at`, and `updated_at`.
The endpoint records an action reported by an authenticated caller; it does not
verify WAVS signatures or establish that the action has executed on-chain.

### Record submission

`POST /v1/actions/{id}/submission` takes no body and moves a `received` action to
`submitting`, setting `submitted_at`. It returns the action with `200`. Repeated
reports while already `submitting` return the unchanged record without adding
history. This reports submission intent; it does not grant exclusive permission
to broadcast or submit a transaction itself.

### Report a result

`POST /v1/actions/{id}/result` accepts:

```json
{
  "status": "completed",
  "tx_hash": "<64 hexadecimal characters>"
}
```

Allowed outcomes are `completed`, `failed`, and `outcome_unknown`. Completion
requires a transaction hash and no error code or detail. Failure and uncertainty require
an `error_code`; a hash is optional. Hashes are stored in uppercase, and an
existing hash is preserved when omitted. Conflicting hashes are rejected.
Optional `error_detail` provides a caller-sanitized explanation of 1–1,024 characters.
It is saved on the action and its transition history; omitting it in a later accepted
result clears the current detail.

Results require a `submitting` or `outcome_unknown` action, except that a later
success report can move `failed` to `completed`. Unknown outcomes can resolve to
completion or failure. Completed actions cannot be overwritten by later failure
or uncertainty reports, and conflicting transaction hashes remain rejected. Identical
repeated results return `200` without modifying timestamps or adding history.
Additional hash information for an unknown result is recorded with a history entry.
A completion discovered through the handled-event query may have no transaction
hash; a later successful callback can supply it without reopening the action.
Out-of-order reports, incompatible terminal results, and attempts to restart an
action return `409 action_conflict`.

These endpoints record authenticated caller reports; they do not independently
confirm the chain outcome. Callers must distinguish definite failures from
ambiguous errors. In particular, insufficient quorum is not a confirmed execution
failure. Detailed operator outcome rules are separate from this API.

### Read action status and history

`GET /v1/actions/{id}` returns `{"action": {...}, "history": [...]}`. Mailbox
information stays internal. History is
ordered by increasing transition ID and contains `id`, `action_id`, `status`,
`error_code`, `error_detail`, and `occurred_at`. The action and history are read consistently.

### List a user's actions

`GET /v1/actions?user_id=<64 lowercase hex characters>&limit=50&before_id=<id>`
returns `{"actions": [...], "next_before_id": ...}`. Only `user_id` is required.
Results are newest first by action ID, with a default page size of 50 and a maximum
of 100. Pass `next_before_id` as `before_id` for the next page; `null` means there
are no further results. Newer inserts do not shift subsequent pages.

All action endpoints require the API key and use the configured chain and handler.
An action outside that namespace is treated as missing (`404 action_not_found`).
Malformed IDs, pagination, hashes, or result combinations return `400`; unsupported
result statuses or unknown result-body fields return `422`. No new registration
queries or action retry operations are added.

### Mailbox acknowledgment

Each mailbox message has its own acknowledgment state:

- `waiting`: the action has no final outcome yet.
- `pending`: the action completed, failed, or closed as unknown; acknowledgment is due.
- `acknowledged`: the message has been acknowledged.
- `obsolete`: UIDVALIDITY changed; its old identifiers cannot be used safely.

Unresolved `outcome_unknown` actions remain waiting. Once reconciliation closes
an unknown outcome, its messages become pending. Later delivered copies attached
to a closed action are also immediately eligible for acknowledgment.

Acknowledgment failures can be retried without resubmitting the action. This state
is internal to the backend and has no HTTP endpoints.

Backend startup requires the same
`WAVS_ENV_MAIL_CREDENTIAL_KIND=plain-imap` and `WAVS_ENV_IMAP_*` settings as the
operators. Operators fetch with `BODY.PEEK[]`, including through `read-mail`.

The worker marks pending messages read by UID, checking the folder's UIDVALIDITY
first. A changed UIDVALIDITY makes the old record `obsolete`, saves
`mailbox_uid_validity_changed`, and stops retries without marking a reused UID
read. Operators can register the email under its new identifiers. A removed
message needs no further acknowledgment. Temporary failures retry with
increasing delays, capped at one hour. Pending work survives backend restarts;
repeating a flag update after a crash is safe.

Keep the receiving account consistent between backend and operators. This setup
supports one receiving account. Mailbox errors are stored as safe error codes.

## Action reconciliation

The backend always runs a read-only reconciliation worker. It uses the configured
chain from WAVS and `BACKEND_SERVICE_HANDLER_ADDRESS`; no signer is required.
Connections are established when checks run, so a temporary RPC outage does not
prevent backend startup.

- `received` past the grace period becomes `failed` with `submission_not_started`.
- With a hash, confirmed transaction success becomes `completed` and confirmed
  transaction failure becomes `failed`.
- Without a hash, or when the transaction is explicitly not found, the worker
  checks the contract's `event_processed` query. A handled event is `completed`.
- Otherwise it keeps checking until the grace period expires, then closes the
  action as `outcome_unknown`. RPC errors and timeouts defer closure, even after
  the grace period. Transaction absence alone is not a confirmed failure.

`BACKEND_RECONCILIATION_INTERVAL_SECONDS` defaults to 30 and
`BACKEND_RECONCILIATION_GRACE_SECONDS` to 900 (15 minutes).
The grace period starts at creation for `received` and at submission intent for
submitted actions; callback updates do not extend it.

`closed_at` distinguishes closed unknown outcomes from unresolved ones. Closure,
history, and mailbox acknowledgment eligibility are saved atomically. Scheduling
survives restarts, and concurrent callbacks invalidate stale worker observations.
A later confirmed success may still resolve a closed unknown outcome. The worker
never resubmits transactions or restarts actions.

## WAVS aggregator integration

Set `BACKEND_API_URL` to the backend's base URL as reached from the WAVS host
(default `http://127.0.0.1:8080`) and use the same `BACKEND_API_KEY` as the backend.
The Docker startup task forwards them as `WAVS_ENV_BACKEND_API_URL` and
`WAVS_ENV_BACKEND_API_KEY`.

The email reader registers each mailbox message through the action creation
endpoint before returning its response to WAVS. It uses WAVS's event ID for the
same event salt, so the aggregator finds the same action. Mailbox identifiers
remain internal and do not enter the signed on-chain payload. API failures leave
the message unread and uncached for another poll. After an operator restart,
terminal or uncertain actions are cached again without requesting another
submission. Metadata remains optional on the API for aggregator callbacks.

The backend's chain and service-handler settings must match the components'
`CHAIN` and `SERVICE_HANDLER_CONTRACT_ADDRESS`. The component rejects mismatched
action records. For operators on another machine, configure a reachable backend
URL and listener rather than the localhost default.

Before returning a submit instruction, the aggregator uses WAVS's event ID and
operator payload to create the action and record submission intent. API errors
stop that instruction; the component does not submit without a durable record.
An already `submitting` action still accepts operator signatures into WAVS's quorum
collection. Completed, failed, and unknown actions do not request another submission.
The API calls themselves are not an exclusive broadcast claim; the service handler
continues to enforce duplicate-execution protection on-chain.

The pinned WAVS Cosmos runtime waits for execution confirmation before its submit
callback. Confirmed success reports `completed` with the hash. Its explicit
`Cosmos Client: tx failed with code: ...` errors report `failed`; replay errors
remain unknown because another submitter may have executed the event. Other errors
report `outcome_unknown`. Reports include a stable `error_code` and a safe
`error_detail`: explicit rejections include the numeric chain code and a bounded
codespace; uncertain outcomes use a generic explanation. Raw logs and transport
messages are not stored.
Insufficient-quorum callbacks leave the action `submitting` while WAVS waits for
additional signatures.

Callbacks recover the database action ID through idempotent creation, so no local
ID cache is required. If API reporting fails after submission, the component logs
an error and returns a callback error without requesting another transaction.
The reconciliation worker resolves or closes the saved action without retrying
submission. A cached email may remain unread while that action is unresolved;
closure makes it eligible for acknowledgment. If callback reporting fails, the
reconciliation worker checks the chain to recover the outcome.

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

`registrations.rs` covers registration API, persistence, and worker behavior.
`actions.rs` covers action API, persistence, database constraints, and acknowledgment
worker recovery. `reconciliation.rs` covers chain results, grace periods, RPC
outages, concurrent callbacks, and atomic closure. Local fake IMAP tests check
flag updates and UIDVALIDITY safety.
Each Postgres test applies all migrations to a fresh test database.

Component tests run without a database or chain:

```sh
cargo test -p app-component-aggregator-submitter -p app-component-operator-email-reader \
  -p app-component-backend-client --lib
cargo clippy -p app-component-aggregator-submitter --all-targets -- -D warnings
cargo build -p app-component-aggregator-submitter --target wasm32-wasip2
```

These tests exercise polling and restart behavior, reporting with a simulated
backend, request encoding, and callback classification. They do not run the WAVS
host or real chain submission.

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
