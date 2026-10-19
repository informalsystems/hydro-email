CREATE TABLE actions (
    id BIGSERIAL PRIMARY KEY,
    chain_key TEXT NOT NULL,
    service_handler_address TEXT NOT NULL,
    event_id TEXT NOT NULL CHECK (event_id ~ '^[0-9a-f]{40}$'),
    user_id TEXT NOT NULL CHECK (user_id ~ '^[0-9a-f]{64}$'),
    payload JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'received' CHECK (status IN (
        'received', 'submitting', 'pending',
        'completed', 'failed', 'outcome_unknown'
    )),
    error_code TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (chain_key, service_handler_address, event_id)
);

CREATE INDEX actions_user_history
    ON actions (chain_key, service_handler_address, user_id, id DESC);

CREATE TABLE action_submission_attempts (
    id BIGSERIAL PRIMARY KEY,
    action_id BIGINT NOT NULL REFERENCES actions (id),
    attempt_number BIGINT NOT NULL CHECK (attempt_number > 0),
    status TEXT NOT NULL DEFAULT 'submitting' CHECK (status IN (
        'submitting', 'pending', 'completed', 'failed', 'outcome_unknown'
    )),
    tx_hash TEXT,
    error_code TEXT,
    submitted_at TIMESTAMPTZ,
    last_checked_at TIMESTAMPTZ,
    next_check_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (action_id, attempt_number),
    CHECK (status <> 'pending' OR tx_hash IS NOT NULL)
);

CREATE INDEX action_submission_attempts_reconciliation
    ON action_submission_attempts (next_check_at, id)
    WHERE status IN ('submitting', 'pending', 'outcome_unknown');

CREATE TABLE action_transitions (
    id BIGSERIAL PRIMARY KEY,
    action_id BIGINT NOT NULL REFERENCES actions (id),
    attempt_number BIGINT,
    status TEXT NOT NULL CHECK (status IN (
        'received', 'submitting', 'pending',
        'completed', 'failed', 'outcome_unknown'
    )),
    error_code TEXT,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    FOREIGN KEY (action_id, attempt_number)
        REFERENCES action_submission_attempts (action_id, attempt_number)
);

CREATE INDEX action_transitions_history
    ON action_transitions (action_id, id);
