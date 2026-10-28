## Set environment

In the `.env`, make sure to set `DEPLOY_*` as needed

## Upload and deploy just the artifacts

```
task deploy:artifacts
task deploy:service
```

## For each user (change `DEPLOY_REGISTER_USER_EMAIL` in the `.env`):

```
task deploy:user
```

## Make sure each backend service is running (per-node)

Note that you'll need to make sure the `.env` is setup for the remote node so it gets the right ipfs gateway url

.env
```
DEPLOY_CHAIN_TARGET="mainnet"
DEPLOY_WAVS_TARGET="remote"
DEPLOY_IPFS_TARGET="remote"
REMOTE_IPFS_GATEWAY_URL="<your ipfs gateway url>"
```

Then, for each node:

```
task backend:start-wavs
```

## Register it for each operator (per-operator)

Easiest is just to set `WAVS_OPERATOR_SIGNING_MNEMONIC_1` and `REMOTE_WAVS_OPERATOR_URL_1` and call this remotely, replacing the env vars for each operator:

```
task deploy:operator
```

## Hydro control-center registry

Set `HYDRO_COMMITTEE_ADDRESS` in the deployment `.env` to the registry's application
admin. `DEPLOY_CONTROL_CENTER_ADDRESS` remains the space-separated initial list
of control centers on the target chain. Deployment uploads the registry alongside
the proxy and instantiates it before creating the sample proxy. The registry has
no chain-level migration admin; the committee controls its application settings.
