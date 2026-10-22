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
    closed_at TIMESTAMPTZ,
    last_checked_at TIMESTAMPTZ,
    next_check_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (chain_key, service_handler_address, event_id),
    CHECK (closed_at IS NULL OR status IN ('completed', 'failed', 'outcome_unknown'))
);

CREATE INDEX actions_user_history
    ON actions (chain_key, service_handler_address, user_id, id DESC);

CREATE INDEX actions_reconciliation
    ON actions (chain_key, service_handler_address, next_check_at, id)
    WHERE closed_at IS NULL AND status IN ('received', 'submitting', 'outcome_unknown');

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

CREATE TABLE action_mailbox_messages (
    id BIGSERIAL PRIMARY KEY,
    action_id BIGINT REFERENCES actions (id),
    rejection_chain_key TEXT,
    rejection_service_handler_address TEXT,
    rejection_code TEXT CHECK (rejection_code ~ '^[a-z0-9_]{1,64}$'),
    folder TEXT NOT NULL CHECK (char_length(folder) BETWEEN 1 AND 255),
    uid_validity BIGINT NOT NULL CHECK (uid_validity BETWEEN 1 AND 4294967295),
    uid BIGINT NOT NULL CHECK (uid BETWEEN 1 AND 4294967295),
    acknowledgment_status TEXT NOT NULL DEFAULT 'waiting'
        CHECK (acknowledgment_status IN ('waiting', 'pending', 'acknowledged', 'obsolete')),
    attempt_count BIGINT NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    last_attempt_at TIMESTAMPTZ,
    next_attempt_at TIMESTAMPTZ,
    last_error_code TEXT CHECK (last_error_code ~ '^[a-z0-9_]{1,64}$'),
    acknowledged_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- Action-linked messages derive scope through the action. Rejections have no action.
    CHECK (
        (action_id IS NOT NULL AND rejection_chain_key IS NULL
         AND rejection_service_handler_address IS NULL AND rejection_code IS NULL)
        OR
        (action_id IS NULL AND rejection_chain_key IS NOT NULL
         AND rejection_service_handler_address IS NOT NULL AND rejection_code IS NOT NULL
         AND acknowledgment_status <> 'waiting')
    ),
    UNIQUE (folder, uid_validity, uid),
    CHECK ((acknowledgment_status = 'acknowledged') = (acknowledged_at IS NOT NULL)),
    CHECK ((acknowledgment_status = 'pending') = (next_attempt_at IS NOT NULL))
);

CREATE INDEX action_mailbox_messages_action ON action_mailbox_messages (action_id, id);
CREATE INDEX action_mailbox_messages_pending
    ON action_mailbox_messages (next_attempt_at, id)
    WHERE acknowledgment_status = 'pending';
