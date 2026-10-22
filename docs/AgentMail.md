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

Each poll checks the unread message identifiers and processes at most one email,
oldest first. Each operator keeps its own in-memory cache to skip emails it has
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

### AgentMail REST API — not yet implemented

AgentMail also offers a REST API (`https://api.agentmail.to/v0`) with Bearer-token
auth. The codebase has a placeholder `agentmail-rest-api` credential kind
(`WAVS_ENV_AGENTMAIL_API_KEY`, `WAVS_ENV_AGENTMAIL_INBOX_ID`,
`WAVS_ENV_AGENTMAIL_BASE_URL`) wired through config/deploy, but selecting it
currently returns a "not implemented" error — it still needs a cursor/dedup
strategy for tracking which messages have already been processed, since the
REST API has no confirmed "unread" flag equivalent to IMAP's `\Seen`. Use
`plain-imap` for now.
