-- Chain + registry scope prevents local/testnet/mainnet registrations colliding.
CREATE TABLE registrations (
    chain_key TEXT NOT NULL,
    registry_address TEXT NOT NULL,
    user_id TEXT NOT NULL CHECK (user_id ~ '^[0-9a-f]{64}$'),
    email TEXT NOT NULL,
    attempt_number BIGINT NOT NULL DEFAULT 1 CHECK (attempt_number > 0),
    submission_stage TEXT CHECK (submission_stage IN ('creating_proxy', 'registering')),
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN (
        'queued', 'creating_proxy', 'proxy_pending', 'proxy_created',
        'registering', 'registration_pending', 'completed', 'failed', 'unknown'
    )),
    proxy_address TEXT,
    create_tx_hash TEXT,
    register_tx_hash TEXT,
    error_code TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    next_check_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_key, registry_address, user_id)
);

CREATE INDEX registrations_work ON registrations (chain_key, registry_address, next_check_at, created_at)
    WHERE status NOT IN ('completed', 'failed');

-- Every transition is persisted atomically with the current registration state.
-- Do not store verification codes, credentials, or raw RPC errors in this log.
CREATE TABLE registration_transitions (
    id BIGSERIAL PRIMARY KEY,
    chain_key TEXT NOT NULL,
    registry_address TEXT NOT NULL,
    user_id TEXT NOT NULL,
    attempt_number BIGINT NOT NULL,
    status TEXT NOT NULL,
    error_code TEXT,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (chain_key, registry_address, user_id)
        REFERENCES registrations (chain_key, registry_address, user_id)
);
CREATE INDEX registration_requests_quota ON registration_transitions (chain_key, occurred_at)
    WHERE status = 'queued';

-- Only one submission per stage per explicitly requested registration attempt.
CREATE TABLE registration_attempts (
    chain_key TEXT NOT NULL,
    registry_address TEXT NOT NULL,
    user_id TEXT NOT NULL,
    attempt_number BIGINT NOT NULL,
    stage TEXT NOT NULL CHECK (stage IN ('creating_proxy', 'registering')),
    tx_hash TEXT,
    attempted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_key, registry_address, user_id, attempt_number, stage),
    FOREIGN KEY (chain_key, registry_address, user_id)
        REFERENCES registrations (chain_key, registry_address, user_id)
);
