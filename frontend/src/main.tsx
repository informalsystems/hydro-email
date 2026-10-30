import { useCallback, useEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { api, ApiError, message, type Registration, type Action, type ActionPage, type Details, type Config } from './api';
import { buildSubject, mailto } from './commands.mjs';
import { formatAtom, fetchBalances } from './balances.mjs';
import './style.css';

const STORAGE = 'hydro-email-test-users-v1';
function savedUsers(): string[] {
  try {
    const values: unknown = JSON.parse(localStorage.getItem(STORAGE) ?? '[]');
    return Array.isArray(values) ? values.filter((v): v is string => typeof v === 'string' && v.length <= 254).slice(0, 50) : [];
  } catch { return []; }
}
function date(value: string | null) { return value ? new Date(value).toLocaleString() : '—'; }
function Status({ status }: { status: string }) {
  return <span className={`status ${status}`}>{status === 'outcome_unknown' || status === 'unknown' ? 'Outcome unresolved' : status.replaceAll('_', ' ')}</span>;
}
function Value({ label, value }: { label: string; value: string | null | undefined }) {
  const [copied, setCopied] = useState(false);
  return <div className="value"><span>{label}</span><div><code>{value || '—'}</code>{value && <button className="copy" aria-label={`Copy ${label}`} onClick={async () => {
    try { await navigator.clipboard.writeText(value); setCopied(true); setTimeout(() => setCopied(false), 1500); }
    catch { setCopied(false); }
  }}>{copied ? 'Copied' : 'Copy'}</button>}</div></div>;
}
// Timers stop on unmount; hidden tabs skip reads and refresh when shown again.
function usePolling(read: () => Promise<void>, enabled: boolean) {
  useEffect(() => {
    if (!enabled) return;
    let running = false;
    const tick = async () => {
      if (document.hidden || running) return;
      running = true;
      try { await read(); } finally { running = false; }
    };
    const timer = setInterval(tick, 5000);
    const visible = () => { if (!document.hidden) void tick(); };
    document.addEventListener('visibilitychange', visible);
    return () => { clearInterval(timer); document.removeEventListener('visibilitychange', visible); };
  }, [read, enabled]);
}

function ProxyFunds({ proxyAddress }: { proxyAddress: string }) {
  const [result, setResult] = useState<{ denom: string; amount: string }[] | null>(null);
  const [error, setError] = useState('');
  const [checked, setChecked] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const alive = useRef(true);
  const inFlight = useRef(false);
  useEffect(() => () => { alive.current = false; }, []);
  const refresh = useCallback(async () => {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(true);
    try {
      const balances = await fetchBalances(proxyAddress);
      if (alive.current) { setResult(balances); setChecked(new Date().toISOString()); setError(''); }
    } catch (reason) { if (alive.current) setError(message(reason)); }
    finally { inFlight.current = false; if (alive.current) setBusy(false); }
  }, [proxyAddress]);
  useEffect(() => { void refresh(); }, [refresh]);
  usePolling(refresh, true);
  const uatom = result?.find(coin => coin.denom === 'uatom')?.amount ?? '0';
  const other = result?.filter(coin => coin.denom !== 'uatom') ?? [];
  return <div className="proxy-funds"><div className="section-title"><h3>Proxy balances · Cosmos Hub</h3><button className="copy" disabled={busy} onClick={() => void refresh()}>{busy ? 'Checking…' : 'Refresh balances'}</button></div>
    {error && <p className="error" role="alert">{error}{result && ' Last successful balance is shown below; it may be stale.'}</p>}
    {!result && !error && <p className="hint">Reading chain balances…</p>}
    {result && <>
      <Value label="ATOM available to deposit" value={`${formatAtom(uatom)} ATOM (${uatom} uatom)`} />
      {uatom === '0' && <p className="hint">No uatom is currently held by the proxy.</p>}
      <p className="hint">Last checked {date(checked)}. Updates every 5 seconds in a visible tab.</p>
      {other.length > 0 && <details className="token-balances"><summary>Other proxy tokens · {other.length}</summary>
        <p className="hint">For vault withdrawals, use the vault share denomination and share amount, not uatom.</p>
        {other.map(coin => <div className="token-balance" key={coin.denom}><Value label="Token denomination" value={coin.denom} /><Value label="Balance in base units" value={coin.amount} /></div>)}
      </details>}
    </>}
  </div>;
}

function Composer({ email, config, ready }: { email: string; config: Config | null; ready: boolean }) {
  const [command, setCommand] = useState('deposit');
  const [address, setAddress] = useState('');
  const [denom, setDenom] = useState('');
  const [amount, setAmount] = useState('');
  const withdraw = command.startsWith('withdraw');
  let subject = '', link = '', error = '';
  try { subject = buildSubject(command, address.trim(), denom.trim(), amount.trim()); link = mailto(config?.inbox ?? '', subject); }
  catch (reason) { error = message(reason); }
  return <section className="card"><div className="section-title"><h2>Compose an action</h2><span className="step">02</span></div>
    <p className="muted">Prepare the subject, then send it from your mail app.</p>
    <label>Action<select value={command} onChange={e => setCommand(e.target.value)}>
      <option value="deposit">Deposit</option><option value="forward">Forward (same as deposit)</option>
      <option value="withdraw">Withdraw from vault (redeem shares)</option><option value="withdraw_receipt">Transfer receipt tokens</option>
    </select></label>
    {withdraw ? <><label>Destination address<input value={address} onChange={e => setAddress(e.target.value)} placeholder="neutron1…" /></label>
      <div className="fields"><label>Vault share denomination<input value={denom} onChange={e => setDenom(e.target.value)} placeholder="Vault receipt token denom" /></label>
      <label>Share amount in base units<input inputMode="numeric" value={amount} onChange={e => setAmount(e.target.value)} placeholder="1000000" /></label></div>
      <p className="hint">{command === 'withdraw' ? 'Redeems vault shares for underlying tokens sent to the destination. Use the share token held by the proxy, not uatom. The payout depends on the vault exchange rate and may be queued.' : 'Sends the receipt/share tokens from the proxy to the destination without redeeming them for underlying tokens.'} Enter a positive integer share amount. Copy the denomination and check its balance under “Other proxy tokens”.</p></> :
      <p className="hint">Forwards available proxy funds to Hydro. Fund the proxy first; this command has no amount field. Completion does not guarantee that a nonzero amount moved.</p>}
    <div className="mail-preview"><Value label="Send from" value={email} /><Value label="Send to" value={config?.inbox} /><Value label="Subject" value={subject} /></div>
    <p className="hint">Choose <strong>{email}</strong> as the sender in your mail app. Leave the subject exactly as shown. The body can be empty.</p>
    {!ready && <p className="notice">Complete registration before sending an action.</p>}
    {error && <p className="hint">{error}</p>}
    {ready && link ? <a className="button primary" href={link} target="_blank" rel="noopener noreferrer">Open email draft <span aria-hidden="true">↗</span></a> : <button disabled>Open email draft</button>}
    <p className="hint">Opening a draft does not send it or create an action record.</p>
  </section>;
}

function ActionDetails({ id }: { id: number }) {
  const [details, setDetails] = useState<Details | null>(null);
  const [error, setError] = useState('');
  const alive = useRef(true);
  useEffect(() => () => { alive.current = false; }, []);
  const refresh = useCallback(async () => {
    try { const result = await api<Details>(`/actions/${id}`); if (alive.current) { setDetails(result); setError(''); } }
    catch (reason) { if (alive.current) setError(message(reason)); }
  }, [id]);
  useEffect(() => { void refresh(); }, [refresh]);
  usePolling(refresh, !!details && !['completed', 'failed'].includes(details.action.status));
  return <div className="action-detail">
    {error && <p className="error" role="alert">{error} <button onClick={() => void refresh()}>Retry</button></p>}
    {!details ? <p>Loading action…</p> : <>
      <Value label="Transaction hash" value={details.action.tx_hash} />
      <Value label="Event ID" value={details.action.event_id} />
      <Value label="Service handler" value={details.action.service_handler_address} />
      <div className="timestamps">Submitted: {date(details.action.submitted_at)}<br />Last checked: {date(details.action.last_checked_at)}<br />Closed: {date(details.action.closed_at)}</div>
      {details.action.status === 'outcome_unknown' && <p className="notice">Execution is unresolved, even if a closed time is present. Later reconciliation may resolve it.</p>}
      <h3>Payload</h3><pre>{JSON.stringify(details.action.payload, null, 2)}</pre>
      <h3>Status history</h3><ol className="history">{details.history.map(entry => <li key={entry.id}><div><Status status={entry.status} /><time>{date(entry.occurred_at)}</time></div>
        {entry.error_code && <code>{entry.error_code}</code>}{entry.error_detail && <p>{entry.error_detail}</p>}</li>)}</ol>
    </>}
  </div>;
}

function Actions({ userId }: { userId: string }) {
  const [page, setPage] = useState<ActionPage | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [expanded, setExpanded] = useState<number | null>(null);
  const [checked, setChecked] = useState<string | null>(null);
  const alive = useRef(true);
  const olderLoaded = useRef(false);
  useEffect(() => () => { alive.current = false; }, []);
  const refresh = useCallback(async () => {
    try {
      const latest = await api<ActionPage>(`/actions?${new URLSearchParams({ user_id: userId, limit: '20' })}`);
      if (!alive.current) return;
      setPage(previous => {
        if (!previous || !olderLoaded.current) return latest;
        const records = new Map(previous.actions.map(action => [action.id, action]));
        latest.actions.forEach(action => records.set(action.id, action));
        return { actions: [...records.values()].sort((a, b) => b.id - a.id), next_before_id: previous.next_before_id };
      });
      setChecked(new Date().toISOString()); setError('');
    } catch (reason) { if (alive.current) setError(message(reason)); }
  }, [userId]);
  useEffect(() => { void refresh(); }, [refresh]);
  usePolling(refresh, true);
  async function older() {
    if (!page?.next_before_id) return;
    setBusy(true);
    try {
      const result = await api<ActionPage>(`/actions?${new URLSearchParams({ user_id: userId, limit: '20', before_id: String(page.next_before_id) })}`);
      if (!alive.current) return;
      olderLoaded.current = true;
      setPage(previous => ({ actions: [...new Map([...(previous?.actions ?? []), ...result.actions].map(action => [action.id, action])).values()].sort((a, b) => b.id - a.id), next_before_id: result.next_before_id }));
      setError('');
    } catch (reason) { if (alive.current) setError(message(reason)); }
    finally { if (alive.current) setBusy(false); }
  }
  function subject(action: Action) {
    const payload = action.payload as { email?: { subject?: string } } | null;
    return typeof payload?.email?.subject === 'string' ? payload.email.subject : 'Email action';
  }
  return <section className="card actions"><div className="section-title"><h2>Action history</h2><button onClick={() => void refresh()}>Refresh</button></div>
    <p className="muted">Updates every 5 seconds while this tab is visible.{checked && <> Last checked {new Date(checked).toLocaleTimeString()}.</>}</p>
    {error && <p className="error" role="alert">{error}</p>}
    {!page && !error && <p>Loading actions…</p>}
    {page?.actions.length === 0 && <div className="empty"><h3>No actions recorded yet</h3><p>After sending an email, wait for the reader to process it. An empty list does not tell us whether an email was rejected.</p></div>}
    {page?.actions.map(action => <article className="action" key={action.id}>
      <button className="action-row" aria-expanded={expanded === action.id} onClick={() => setExpanded(expanded === action.id ? null : action.id)}>
        <span className="action-id">#{action.id}</span><span className="action-subject">{subject(action)}<small>{date(action.created_at)}</small></span>
        <Status status={action.status} /><span aria-hidden="true">{expanded === action.id ? '−' : '+'}</span>
      </button>
      {(action.error_code || action.error_detail) && <p className="action-error">{action.error_code}{action.error_detail && `: ${action.error_detail}`}</p>}
      {expanded === action.id && <ActionDetails key={action.id} id={action.id} />}
    </article>)}
    {page?.next_before_id && <button className="load" disabled={busy} onClick={() => void older()}>{busy ? 'Loading…' : 'Load older actions'}</button>}
  </section>;
}

function Workspace({ email, config }: { email: string; config: Config | null }) {
  const [registration, setRegistration] = useState<Registration | null | undefined>();
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [retryAccepted, setRetryAccepted] = useState(false);
  const alive = useRef(true);
  useEffect(() => () => { alive.current = false; }, []);
  const lookup = useCallback(async () => {
    try {
      const record = await api<Registration>(`/registrations/lookup?${new URLSearchParams({ email })}`);
      if (alive.current) { setRegistration(record); setError(''); }
    } catch (reason) {
      if (!alive.current) return;
      if (reason instanceof ApiError && reason.status === 404 && reason.code === 'registration_not_found') { setRegistration(null); setError(''); }
      else setError(message(reason));
    }
  }, [email]);
  useEffect(() => { void lookup(); }, [lookup]);
  const refreshStatus = useCallback(async () => {
    if (!registration) return;
    try { const result = await api<Registration>(`/registrations/${registration.user_id}`); if (alive.current) { setRegistration(result); setError(''); } }
    catch (reason) { if (alive.current) setError(message(reason)); }
  }, [registration?.user_id]);
  const terminal = registration && ['completed', 'failed', 'unknown'].includes(registration.status);
  usePolling(refreshStatus, !!registration && !terminal);
  async function register() {
    setBusy(true); setError('');
    try {
      const record = await api<Registration>('/registrations', { method: 'POST', body: JSON.stringify({ email }) });
      if (alive.current) { setRegistration(record); setRetryAccepted(false); }
    } catch (reason) { if (alive.current) setError(message(reason)); }
    finally { if (alive.current) setBusy(false); }
  }
  const retry = registration && ['failed', 'unknown'].includes(registration.status);
  return <>
    <div className="workspace-title"><div><span className="eyebrow">Selected user</span><h1>{email}</h1></div>{registration && <Status status={registration.status} />}</div>
    <div className="columns"><section className="card"><div className="section-title"><h2>Registration & proxy</h2><span className="step">01</span></div>
      {error && <p className="error" role="alert">{error} <button onClick={() => void lookup()}>Check again</button></p>}
      {registration === undefined && !error && <p>Checking registration…</p>}
      {registration === null && <><p className="muted">No registration exists for this email in the configured backend.</p><button className="primary" disabled={busy} onClick={() => void register()}>{busy ? 'Registering…' : 'Register email'}</button><p className="hint">Registration can spend gas when provisioning is enabled.</p></>}
      {registration && <>
        <Value label="User ID" value={registration.user_id} />
        <div className={registration.status === 'completed' ? 'proxy ready' : 'proxy'}><Value label={registration.status === 'completed' ? 'Proxy address · ready to fund' : 'Proxy address · awaiting completion'} value={registration.proxy_address} /></div>
        {registration.status === 'completed' && registration.proxy_address && <ProxyFunds key={registration.proxy_address} proxyAddress={registration.proxy_address} />}
        {registration.status !== 'completed' && <p className="notice">Wait for completed registration before funding the proxy or sending action emails. An address can be assigned before the proxy exists.</p>}
        <Value label="Registration transaction" value={registration.tx_hash} />
        <p className="hint">Attempt {registration.attempt_number} · Updated {date(registration.updated_at)}</p>
        {registration.error_code && <p className="error">{registration.error_code}</p>}
        {retry && <><p className="notice">{registration.status === 'unknown' ? 'The provisioning outcome is unresolved. ' : ''}Retrying may spend additional gas.</p>
          <label className="checkbox"><input type="checkbox" checked={retryAccepted} onChange={e => setRetryAccepted(e.target.checked)} />I want to retry provisioning.</label>
          <button disabled={busy || !retryAccepted} onClick={() => void register()}>{busy ? 'Queueing…' : 'Retry registration'}</button></>}
        <button className="subtle" disabled={busy} onClick={() => void refreshStatus()}>Refresh status</button>
      </>}
    </section><Composer email={email} config={config} ready={registration?.status === 'completed'} /></div>
    {registration && <Actions key={registration.user_id} userId={registration.user_id} />}
  </>;
}

function App() {
  const [users, setUsers] = useState(savedUsers);
  const [selected, setSelected] = useState(() => savedUsers()[0] ?? '');
  const [email, setEmail] = useState('');
  const [config, setConfig] = useState<Config | null>(null);
  const [configError, setConfigError] = useState('');
  const [health, setHealth] = useState('Checking API…');
  const [storageError, setStorageError] = useState('');
  const refreshConfig = useCallback(async () => {
    try { setConfig(await api<Config>('/config')); setConfigError(''); }
    catch (reason) { setConfigError(message(reason)); }
  }, []);
  useEffect(() => { void refreshConfig(); }, [refreshConfig]);
  const checkHealth = useCallback(async () => {
    try { await api('/health'); setHealth('API ready'); } catch { setHealth('API unavailable'); }
  }, []);
  useEffect(() => { void checkHealth(); }, [checkHealth]);
  usePolling(checkHealth, true);
  useEffect(() => {
    try { localStorage.setItem(STORAGE, JSON.stringify(users)); }
    catch { setStorageError('Browser storage unavailable. Users will only be remembered for this session.'); }
  }, [users]);
  return <div className="shell"><header><a className="brand" href="/" aria-label="Hydro Email home"><span className="brand-mark">≈</span><span>hydro<span className="brand-light"> / email</span></span></a>
    <div className="header-meta"><span className="tag">Local test console</span><span className={health === 'API ready' ? 'health good' : 'health'}>{health}</span></div></header>
    <main><div className="intro"><span className="eyebrow">Email → proxy → Hydro</span><h1>Test the email flow.</h1><p>Register a user, prepare an instruction, and follow its execution.</p><span className="chain">{config?.chain ?? 'Loading configuration…'}</span></div>
      {configError && <p className="error" role="alert">{configError} <button onClick={() => void refreshConfig()}>Reload configuration</button></p>}
      <section className="user-bar" aria-label="User selection"><label>Selected email<select value={selected} onChange={e => setSelected(e.target.value)}><option value="">Choose a user</option>{users.map(user => <option key={user} value={user}>{user}</option>)}</select></label>
        <form onSubmit={e => {
          e.preventDefault(); const value = email.trim(); if (!value) return;
          setUsers(previous => [value, ...previous.filter(user => user !== value)].slice(0, 50)); setSelected(value); setEmail('');
        }}><label>Add or look up an email<input type="email" required value={email} onChange={e => setEmail(e.target.value)} placeholder="alice@example.com" maxLength={254} /></label><button type="submit">Look up email <span aria-hidden="true">→</span></button></form>
        {selected && <button className="subtle remove" onClick={() => { setUsers(previous => previous.filter(user => user !== selected)); setSelected(''); }}>Forget</button>}
      </section><p className="hint user-hint">This browser remembers your email list. Looking up an email only reads its registration; “Forget” removes it from this list.</p>
      {storageError && <p className="notice">{storageError}</p>}
      {selected ? <Workspace key={selected} email={selected} config={config} /> : <div className="empty welcome"><h2>Start with an email address</h2><p>Look up an existing user or add an email to register. Then prepare an email action and track its status here.</p></div>}
    </main><footer>Hydro Email · Testing interface <span>Actions are triggered by the emails you send.</span></footer></div>;
}

createRoot(document.getElementById('root')!).render(<App />);
