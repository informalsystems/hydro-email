# Prerequisites

Before getting started, make sure to install all the necessary [Tooling](./Tooling.md)

# First time

These steps may take a while, but it's only needed the first time you dive into the project

1. Copy `.example.env` to `.env` and replace the values
2. Pull all the docker images: `task docker-pull`
3. Download the latest WIT definitions: `task components:fetch-wit`
4. Build the helper binary: `task build-helper`

# Simple flow 

1. Build all contracts, components, and schema

```bash
task build-all
```

2. (local-only) Start all backend services

```bash
task backend:start-all

# Alternatively, if you need more than one operator
# 
# task backend:start-all OPERATORS=3
```

3. (local-only) Tap the faucet for all the mnemonics

```bash
task deploy:tap-faucet-all
```

4. Prepare deployment and initialize its admins

See [Remote](./Remote.md) for more details if deploying remotely

Local
```bash
task deploy:all

# Alternatively, skip uploading contracts, middleware, and/or components
# this assumes they were already uploaded before and the output files exist
# 
# task deploy:all SKIP_UPLOAD_CONTRACTS=true SKIP_UPLOAD_COMPONENTS=true SKIP_UPLOAD_MIDDLEWARE=true
```

Set `HYDRO_COMMITTEE_ADDRESS` (the final owner of both middleware contracts) and
`DEPLOY_USER_REGISTRY_ADMIN_ADDRESS` in `.env`. Use valid target-chain addresses,
including local development. `deploy:all` deploys the contracts, configures
URI/quorum, loads operators, and registers their keys/weights using `CLI_MNEMONIC`.
It then verifies setup and atomically transfers both middleware authorities to
the committee. No committee transaction or additional admin mnemonic is needed
for bootstrap. Pass the desired operator count, for example `OPERATORS=2`.

Normal users register through the API using its dedicated wallet. The optional
`task deploy:user` prepares a sample registration request; `BROADCAST=true` uses
`BACKEND_SIGNING_MNEMONIC` for that task only. Infrastructure deployment does not
require the API's private mnemonic. See [Remote](./Remote.md) for individual
stages, ownership handoff, and later committee administration.

For `DEPLOY_CHAIN_TARGET="local"`, deployment also builds and deploys Hydro's
control center with an empty vault list, registers it in a new control-center
registry, then points the proxy at that registry.
This lets you test email recording and the proxy's `Forwarded` state without
deploying vaults. It does not test deposits. The local flow ignores
`DEPLOY_CONTROL_CENTER_ADDRESS` and uses the deployment wallet as registry admin.
Remote deployments initialize the registry with `DEPLOY_CONTROL_CENTER_ADDRESS`
and set its admin to `HYDRO_COMMITTEE_ADDRESS`. The committee can subsequently add
or remove control centers in the registry without recreating proxies.

5. Send an email to the operator email address

See [LocalEmail](./LocalEmail.md) for using the local email server for testing, otherwise, use a real email server

6. Check the on-chain results every 10 seconds or so

This will use the same `DEPLOY_CHAIN_TARGET` env var as the deploy commands to determine which chain to query

```bash
task contracts:query-service-handler-emails
task contracts:query-proxy-state
```

7. Iterating is sometimes more convenient by first deleting the active service, before redeploying

```bash
task deploy:operator-delete-service
```

8. (local-only) When done, stop all backend services

```bash
task backend:stop-all
```

Each of these steps can be done individually and with more granularity as needed.

Check the relevant taskfile for more details.

# Execute a component directly

This will execute the operator component to check the latest email 

```bash
task components:exec-read-mail
```

# Testing


## Contracts

All off-chain tests
```bash
task test:off-chain
```

*on-chain*

Make sure you've [started the chains](#chains) first

```bash
task test:on-chain
```

## Components

Just `cargo test` as needed

## Telemetry UIs

- Jaeger UI is at [http://localhost:16686/](http://localhost:16686/)
- Prometheus is at [http://localhost:9092/](http://localhost:9092/)
