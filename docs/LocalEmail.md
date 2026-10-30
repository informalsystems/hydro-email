Sometimes it's helpful to debug against a local mail server instead of using real credentials on Google etc.

## Using Greenmail

Start the local mail server

```bash
task backend:mail-server-start
```

Send an email (then check again!)

```bash
task backend:mail-server-send
```

Now, assuming you've set the `.env` credentials for a local test user, you can check the inbox as usual:

```bash
task components:exec-read-mail
```

This diagnostic reads and marks the message as seen, but does not submit it to the chain.
To test the deployed service, send a fresh message and let the WAVS operator read it
automatically (every 10 seconds). Then query the results:

```bash
task contracts:query-service-handler-emails
task contracts:query-proxy-state
```

When you're finished, you can stop the local mail server:

```bash
task backend:mail-server-stop
```

## Reader limits and failure handling

Commands still come from the subject (`deposit`, `withdraw`, or
`withdraw_receipt`). The reader parses only the top-level headers; MIME parts and
attachments are opaque bytes. DKIM still authenticates the sender, subject,
event identity, and raw body using the existing verification policy.

Messages over 1 MiB, header blocks over 64 KiB, and messages with more than 256
top-level headers are permanently rejected through the backend. IMAP fetches
request at most 1 MiB plus one byte, so oversized mail can be identified without
downloading its entire body. Deep MIME nesting within these limits is safe and
is ignored rather than recursively parsed.

Before fetching a selected UID, the operator records a processing attempt in its
existing mailbox cache. An unfinished attempt delays that UID for 30 seconds so
newer messages can proceed. After three unfinished attempts, the next poll
reports `email_processing_failed` without fetching or parsing it again. Temporary
DKIM lookup failures keep their separate backoff and one-hour expiry. Rejection
must be confirmed by the backend before the message is permanently cached and
acknowledged. As with the existing verification retries, clearing the operator's
host cache resets attempt counts.

A cron invocation skips up to 16 rejected, deferred, or already completed messages
while looking for the next actionable instruction, and emits at most one action.
