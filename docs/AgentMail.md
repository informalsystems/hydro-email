# AgentMail

This project reads mail from an [AgentMail](https://docs.agentmail.to/imap-smtp) inbox over plain IMAP. No OAuth flow is needed.

## 1. Create an AgentMail inbox

Sign up at [AgentMail](https://agentmail.to) and create an inbox (e.g. via the console or the `inboxes.create` API). Note the inbox's email address — this is your IMAP username.

## 2. Create an API key

Create (or copy) an API key from the AgentMail console. This is your IMAP password.

## 3. Configure `.env`

```bash
WAVS_ENV_MAIL_CREDENTIAL_KIND="plain-imap"
WAVS_ENV_IMAP_PORT=993
WAVS_ENV_IMAP_HOST="imap.agentmail.to"
WAVS_ENV_IMAP_TLS=true
WAVS_ENV_IMAP_USERNAME="<your-inbox>@agentmail.to"
WAVS_ENV_IMAP_PASSWORD="<AgentMail API key>"
```

See [.env.example](../.env.example) for the full block, including local-dev (GreenMail) settings.

### Polling and deduplication

Each poll checks unread message identifiers, prioritizes never-attempted messages
before due retries, and uses oldest UID first within each group. It can skip up to
16 rejected, deferred, or already completed messages, emitting at most one action. Each operator keeps its own in-memory cache to skip emails it has
already processed. A successful unread search removes cached entries that are
no longer unread.

Restarting an operator clears its cache, so unread emails may be processed again.
Contract replay protection prevents duplicate successful execution.

Operators fetch with `BODY.PEEK[]`, leaving emails unread for other operators.
The `read-mail` command also leaves them unread.

The backend runs the acknowledgment worker. Give it the same
`WAVS_ENV_MAIL_CREDENTIAL_KIND` and `WAVS_ENV_IMAP_*` settings as the operators;
these settings are required at backend startup.
The backend marks each message read after its action completes or fails. Mailbox
failures are retried after a delay, including after a backend restart.
Unresolved `outcome_unknown` actions remain unread until reconciliation resolves
or closes them. Closed unknown outcomes become eligible to be marked read.

Operators register mailbox identifiers with the backend before returning a signed
action. These identifiers stay out of the signed payload and action responses.

### Verification resource limits

The reader rejects messages with more than three DKIM signatures aligned with the
From domain, before any DNS lookup. Signatures from unrelated domains do not count
toward that limit and cannot authenticate the sender. Existing header-size and
header-count limits also apply.

Service generation sets the reader's WAVS execution limits to nine wall-clock
seconds and one billion fuel per invocation. These defaults are configured in
`taskfile/config.yml` as `EMAIL_READER_TIME_LIMIT_SECONDS` and
`EMAIL_READER_FUEL_LIMIT`. The nine-second limit is shorter than the default
ten-second cron interval; keep the execution limit below the interval if changing
the schedule. Fuel limits computation; the wall-clock limit also covers network
waits. The limits apply to the whole reader invocation, including IMAP and backend
requests. Aggregator limits are unchanged.

A runtime cutoff leaves the processing reservation created before fetching the
message. The message backs off for 30 seconds and is rejected after three
unfinished attempts. DNS failures that return normally retain their existing
verification backoff and one-hour expiry. Fresh-mail priority prevents due retries
from repeatedly jumping ahead, but does not eliminate the cost of first attempts
or guarantee service under an unlimited flood of new mail.

Rebuild the reader and task-helper, upload the reader, and regenerate/update the
service definition for these changes to take effect. Restarting operators alone
does not update an existing IPFS service definition.

### AgentMail REST API — not yet implemented

AgentMail also offers a REST API (`https://api.agentmail.to/v0`) with Bearer-token
auth. The codebase has a placeholder `agentmail-rest-api` credential kind
(`WAVS_ENV_AGENTMAIL_API_KEY`, `WAVS_ENV_AGENTMAIL_INBOX_ID`,
`WAVS_ENV_AGENTMAIL_BASE_URL`) wired through config/deploy, but selecting it
currently returns a "not implemented" error — it still needs a cursor/dedup
strategy for tracking which messages have already been processed, since the
REST API has no confirmed "unread" flag equivalent to IMAP's `\Seen`. Use
`plain-imap` for now.
