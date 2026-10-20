CREATE TABLE actions (
    id BIGSERIAL PRIMARY KEY,
    chain_key TEXT NOT NULL,
    service_handler_address TEXT NOT NULL,
    event_id TEXT NOT NULL CHECK (event_id ~ '^[0-9a-f]{40}$'),
    user_id TEXT NOT NULL CHECK (user_id ~ '^[0-9a-f]{64}$'),
    payload JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'received' CHECK (status IN (
        'received', 'submitting', 'completed', 'failed', 'outcome_unknown'
    )),
    tx_hash TEXT,
    error_code TEXT,
    error_detail TEXT CHECK (char_length(error_detail) BETWEEN 1 AND 1024),
    submitted_at TIMESTAMPTZ,
    last_checked_at TIMESTAMPTZ,
    next_check_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (chain_key, service_handler_address, event_id)
);

CREATE INDEX actions_user_history
    ON actions (chain_key, service_handler_address, user_id, id DESC);

CREATE INDEX actions_reconciliation
    ON actions (chain_key, service_handler_address, next_check_at, id)
    WHERE status IN ('submitting', 'outcome_unknown');

CREATE TABLE action_transitions (
    id BIGSERIAL PRIMARY KEY,
    action_id BIGINT NOT NULL REFERENCES actions (id),
    status TEXT NOT NULL CHECK (status IN (
        'received', 'submitting', 'completed', 'failed', 'outcome_unknown'
    )),
    error_code TEXT,
    error_detail TEXT CHECK (char_length(error_detail) BETWEEN 1 AND 1024),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX action_transitions_history
    ON action_transitions (action_id, id);
