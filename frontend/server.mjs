import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseEnv } from 'node:util';
import path from 'node:path';

const root = fileURLToPath(new URL('.', import.meta.url));
const PORT = 5173;

export async function loadConfig(env = process.env) {
  let file = {};
  try { file = parseEnv(await readFile(path.join(root, '../.env'), 'utf8')); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  // Read an explicit allowlist. Never copy the repository environment into Vite.
  const value = (key, fallback = '') => env[key] ?? file[key] ?? fallback;
  const backendUrl = new URL(value('BACKEND_API_URL', 'http://127.0.0.1:8080'));
  if (!['http:', 'https:'].includes(backendUrl.protocol) || backendUrl.username || backendUrl.password) {
    throw new Error('BACKEND_API_URL must be an HTTP(S) URL without credentials');
  }
  const apiKey = value('BACKEND_API_KEY');
  if (!apiKey) throw new Error('Set BACKEND_API_KEY in the repository .env');
  return {
    backendUrl: backendUrl.origin, apiKey,
    inbox: value('WAVS_ENV_IMAP_USERNAME'),
    chain: value('BACKEND_CHAIN_KEY', 'Unspecified chain'),
  };
}

function json(res, status, body) {
  res.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' });
  res.end(JSON.stringify(body));
}

// Deliberately excludes all operator reporting and mailbox mutation endpoints.
export function upstreamPath(method, url) {
  const queryKeys = [...url.searchParams.keys()];
  if (new Set(queryKeys).size !== queryKeys.length) return null;
  const noQuery = queryKeys.length === 0;
  if (method === 'POST' && url.pathname === '/api/registrations' && noQuery) return '/v1/registrations';
  if (method !== 'GET') return null;
  if (url.pathname === '/api/registrations/lookup' && queryKeys.length === 1 && queryKeys[0] === 'email') {
    return `/v1/registrations/lookup?${url.searchParams}`;
  }
  if (/^\/api\/registrations\/[0-9a-f]{64}$/.test(url.pathname) && noQuery) return url.pathname.replace('/api/', '/v1/');
  if (/^\/api\/actions\/[1-9][0-9]*$/.test(url.pathname) && noQuery) return url.pathname.replace('/api/', '/v1/');
  if (url.pathname === '/api/actions' && queryKeys.every(key => ['user_id', 'limit', 'before_id'].includes(key)) &&
      /^[0-9a-f]{64}$/.test(url.searchParams.get('user_id') ?? '') &&
      /^(?:[1-9][0-9]?|100)$/.test(url.searchParams.get('limit') ?? '20') &&
      /^[1-9][0-9]*$/.test(url.searchParams.get('before_id') ?? '1')) {
    return `/v1/actions?${url.searchParams}`;
  }
  if (url.pathname === '/api/health' && noQuery) return '/readyz';
  return null;
}

export function createHandler(config, fallback, fetchBackend = fetch) {
  return async (req, res) => {
    try {
      const expectedHosts = [`127.0.0.1:${req.socket.localPort}`, `localhost:${req.socket.localPort}`];
      if (!expectedHosts.includes(req.headers.host)) return json(res, 403, { error: 'local_access_only' });
      const url = new URL(req.url, `http://${req.headers.host}`);
      if (!url.pathname.startsWith('/api/')) return fallback(req, res);
      // This custom header forces other browser origins to preflight, which we do not allow.
      if (req.headers['x-hydro-test'] !== '1' ||
          (req.headers.origin && req.headers.origin !== `http://${req.headers.host}`)) {
        return json(res, 403, { error: 'same_origin_required' });
      }
      if (req.method === 'GET' && url.pathname === '/api/config' && !url.search) {
        return json(res, 200, { inbox: config.inbox, chain: config.chain });
      }
      const route = upstreamPath(req.method, url);
      if (!route) return json(res, 404, { error: 'route_not_available' });
      let body;
      if (req.method === 'POST') {
        if (!req.headers['content-type']?.startsWith('application/json')) return json(res, 415, { error: 'json_required' });
        const chunks = [];
        let size = 0;
        for await (const chunk of req) {
          size += chunk.length;
          if (size > 4096) return json(res, 413, { error: 'request_too_large' });
          chunks.push(chunk);
        }
        let input;
        try { input = JSON.parse(Buffer.concat(chunks).toString()); }
        catch { return json(res, 400, { error: 'invalid_json' }); }
        if (!input || typeof input.email !== 'string' || Object.keys(input).length !== 1) {
          return json(res, 400, { error: 'email_required' });
        }
        body = JSON.stringify({ email: input.email });
      }
      const upstream = await fetchBackend(`${config.backendUrl}${route}`, {
        method: req.method, body, redirect: 'error', signal: AbortSignal.timeout(15000),
        headers: { Authorization: `Bearer ${config.apiKey}`, ...(body ? { 'Content-Type': 'application/json' } : {}) },
      });
      if (route === '/readyz') return json(res, upstream.ok ? 200 : 503, { ready: upstream.ok });
      // Backend query-shape errors may be plain text; never reflect arbitrary upstream HTML.
      let result;
      try { result = await upstream.json(); }
      catch { result = { error: upstream.ok ? 'invalid_backend_response' : 'backend_request_failed' }; }
      return json(res, upstream.status, result);
    } catch {
      return json(res, 502, { error: 'backend_unreachable' });
    }
  };
}

async function main() {
  const config = await loadConfig();
  const production = process.argv.includes('--production');
  let fallback;
  let vite;
  if (production) {
    const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml' };
    fallback = async (req, res) => {
      const pathname = new URL(req.url, 'http://localhost').pathname;
      // Only serve the entry point and Vite's generated assets, never repository files.
      if (req.method !== 'GET' || (pathname !== '/' && !/^\/assets\/[\w.-]+$/.test(pathname))) {
        res.writeHead(404); return res.end('Not found');
      }
      try {
        const name = pathname === '/' ? 'index.html' : pathname.slice(1);
        const content = await readFile(path.join(root, 'dist', name));
        res.writeHead(200, { 'Content-Type': mime[path.extname(name)] ?? 'application/octet-stream', 'X-Content-Type-Options': 'nosniff' });
        res.end(content);
      } catch { res.writeHead(404); res.end('Build the frontend first: npm run build'); }
    };
  } else {
    const { createServer: createViteServer } = await import('vite');
    vite = await createViteServer({ root, server: { middlewareMode: true, fs: { strict: true, allow: [root] } }, appType: 'spa' });
    fallback = (req, res) => vite.middlewares(req, res, () => { res.writeHead(404); res.end(); });
  }
  const server = createServer(createHandler(config, fallback));
  server.on('error', () => { console.error('Could not start frontend; check whether its port is already in use.'); process.exit(1); });
  server.listen(PORT, '127.0.0.1', () => console.log(`Hydro Email test console: http://127.0.0.1:${PORT}`));
  const close = async () => { await vite?.close(); server.close(() => process.exit(0)); server.closeAllConnections(); };
  process.on('SIGINT', close);
  process.on('SIGTERM', close);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(() => { console.error('Frontend startup failed. Check .env configuration and installed dependencies.'); process.exitCode = 1; });
}
