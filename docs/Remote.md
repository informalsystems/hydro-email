## Deployment identities

Set these public addresses in `.env`:

```dotenv
HYDRO_COMMITTEE_ADDRESS="<Hydro committee address>"
DEPLOY_USER_REGISTRY_ADMIN_ADDRESS="<dedicated API wallet address>"
```

The committee is the final service-manager admin and stake-registry owner. Use
valid target-chain addresses, including local development. The API wallet address
must match `BACKEND_SIGNING_MNEMONIC`, but normal infrastructure deployment does
not require that mnemonic. `CLI_MNEMONIC` is the only deployment/setup signer.

Both middleware contracts initially belong to the CLI wallet. It sets URI/quorum,
loads operators, and registers their signing keys and voting weights. Only after
setup is verified does it transfer both authorities directly to the Hydro
committee. No DAO proposal or acceptance transaction is needed for initial setup.
The CLI wallet loses these permissions once handoff completes, unless deliberately
also selected as the committee address. All deployed contracts have no CosmWasm
migration admin; operational ownership does not grant upgrade authority.

The local [stake registry](../packages/contracts/stake-registry/UPSTREAM.md) and
[service manager](../packages/contracts/service-manager/UPSTREAM.md) derive from
middleware `v0.2.0-alpha.15`. Build them with `task contracts:build-all`; deployment
uploads these local WASMs instead of the upstream binaries.

## Complete infrastructure deployment

Start WAVS/IPFS services and configure their endpoints first, then run:

```bash
task deploy:all
# For two operators:
# task deploy:all OPERATORS=2
```

The ordered flow is:

1. Deploy contracts with temporary CLI ownership of both middleware contracts.
   The user registry instead receives the dedicated API wallet as its sole admin.
2. Upload the service definition and set its URI/quorum using CLI authority.
3. Load the service on each operator, obtain its public signing key, and register
   that key and its voting weight on-chain using the CLI wallet.
4. Verify the URI, quorum, linked contracts, every expected operator key/weight,
   and the total operator weight. Transfer service-manager admin and stake-registry
   ownership to the committee in one atomic transaction and verify both owners.

If either transfer fails, both transfers roll back. Handoff refuses missing,
mismatched, duplicate, or zero-weight operator entries. Public operator requests
are recorded in `.deployments/middleware-signing-key-<operator number>.json`.
Configuration and final ownership results are recorded in
`.deployments/middleware-configure.json` and `.deployments/middleware-finalize.json`.

User registration is the API's responsibility and is not an infrastructure admin
step. `deploy:all` does not register a sample user or require the API's mnemonic.

## Run individual deployment stages

```bash
task deploy:artifacts
task deploy:service
task deploy:operator OPERATORS=2
task deploy:middleware-finalize OPERATORS=2
```

Use the same operator count throughout. `deploy:service` configures URI/quorum but
retains CLI ownership. `deploy:operator` registers operator keys/weights by default
and verifies the service URI before loading it. `middleware-finalize` reads the
expected public operator requests, verifies them on-chain, then hands over both
roles. Matching existing operator entries and completed handoffs can be verified
without another transaction. After an uncertain submission response, inspect the
chain before retrying a stage; do not blindly redeploy the full environment.

## Later committee administration

After handoff, changing URI/quorum, operator keys/weights, or either middleware
owner requires committee authorization. These tasks can prepare unsigned execute
requests for the DAO/multisig workflow:

```bash
task deploy:middleware-set-service-uri ADDR="<manager>" URI="ipfs://<CID>/service.json"
task deploy:middleware-set-quorum QUORUM_NUMERATOR=2 QUORUM_DENOMINATOR=3
task deploy:operator BROADCAST=false
```

The URI/quorum tasks prepare requests by default. `deploy:operator` normally
broadcasts during bootstrap; `BROADCAST=false` instead prepares requests. Explicit
broadcasts always use `CLI_MNEMONIC`; there is no additional admin mnemonic. Once
the committee owns the contracts, the deployment wallet cannot authorize changes.
A constituent wallet cannot sign as the whole DAO/multisig.

## Optional sample user

Normal users register through the API, which signs with its existing
`BACKEND_SIGNING_MNEMONIC`. To prepare a sample email/proxy mapping from deployment
records, set `DEPLOY_REGISTER_USER_EMAIL` and run:

```bash
task deploy:user
```

This prepares an unsigned registration request. Explicitly running
`task deploy:user BROADCAST=true` uses `BACKEND_SIGNING_MNEMONIC`, whose public
address must be the user-registry admin. This optional task requires the API
credential locally; infrastructure deployment never reads it or grants the CLI
wallet user-registration permissions automatically.

## Start WAVS and IPFS on the deployment host

Use mainnet for contracts and local endpoints for WAVS and IPFS running on the
same host as the deployment commands. This also applies when that host is a
DigitalOcean server; `local` describes the service endpoints, not the chain.

```dotenv
DEPLOY_CHAIN_TARGET="mainnet"
DEPLOY_WAVS_TARGET="local"
DEPLOY_IPFS_TARGET="local"
```

```bash
task backend:start-ipfs
task backend:start-wavs OPERATORS=2
```

Set `WAVS_OPERATOR_SIGNING_MNEMONIC_1` and
`WAVS_OPERATOR_SIGNING_MNEMONIC_2` for the two operators. The tooling uses local
WAVS endpoints and the local IPFS gateway automatically. During bootstrap
`deploy:operator OPERATORS=2` performs service loading and on-chain operator
registration before final committee handoff.

## Hydro control-center registry

For mainnet deployments, `HYDRO_COMMITTEE_ADDRESS` also administers the
control-center registry. `DEPLOY_CONTROL_CENTER_ADDRESS` supplies its
space-separated initial control centers. Deploying to a local chain instead
creates an empty-vault control center and uses the local deployment wallet for
this control-center registry's administration. The
control-center registry has no chain-level migration admin.
