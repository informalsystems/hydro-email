-- Chain + registry scope prevents local/testnet/mainnet registrations colliding.
CREATE TABLE registrations (
    chain_key TEXT NOT NULL,
    registry_address TEXT NOT NULL,
    user_id TEXT NOT NULL CHECK (user_id ~ '^[0-9a-f]{64}$'),
    email TEXT NOT NULL,
    attempt_number BIGINT NOT NULL DEFAULT 1 CHECK (attempt_number > 0),
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN (
        'queued', 'submitting', 'pending', 'completed', 'failed', 'unknown'
    )),
    proxy_address TEXT,
    tx_hash TEXT,
    error_code TEXT,
    activated_at TIMESTAMPTZ,
    activation_error_code TEXT CHECK (activation_error_code ~ '^[a-z0-9_]{1,64}$'),
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

-- Only one submission per explicitly requested registration attempt.
CREATE TABLE registration_attempts (
    chain_key TEXT NOT NULL,
    registry_address TEXT NOT NULL,
    user_id TEXT NOT NULL,
    attempt_number BIGINT NOT NULL,
    tx_hash TEXT,
    attempted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_key, registry_address, user_id, attempt_number),
    FOREIGN KEY (chain_key, registry_address, user_id)
        REFERENCES registrations (chain_key, registry_address, user_id)
);

-- Operator reports are immutable; no raw emails or DKIM credentials are stored.
CREATE TABLE registration_activations (
    id BIGSERIAL PRIMARY KEY,
    chain_key TEXT NOT NULL,
    registry_address TEXT NOT NULL,
    service_handler_address TEXT NOT NULL,
    user_id TEXT NOT NULL,
    verified BOOLEAN NOT NULL,
    error_code TEXT CHECK (error_code ~ '^[a-z0-9_]{1,64}$'),
    folder TEXT NOT NULL,
    uid_validity BIGINT NOT NULL,
    uid BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((verified AND error_code IS NULL) OR (NOT verified AND error_code IS NOT NULL)),
    UNIQUE (folder, uid_validity, uid),
    FOREIGN KEY (chain_key, registry_address, user_id)
        REFERENCES registrations (chain_key, registry_address, user_id)
);
