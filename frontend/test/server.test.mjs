import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer, request } from 'node:http';
import { createHandler, upstreamPath } from '../server.mjs';

const config = { backendUrl: 'http://backend.invalid', apiKey: 'private-test-key', inbox: 'test@example.com', chain: 'test-chain' };

test('the route allowlist blocks internal endpoints, unexpected queries and mutations', () => {
  const url = path => new URL(path, 'http://localhost');
  for (const [method, path] of [
    ['POST', '/api/actions'], ['POST', '/api/actions/1/result'], ['POST', '/api/actions/1/submission'],
    ['POST', '/api/mailbox-rejections'], ['GET', '/api/registrations/lookup?email=a&email=b'],
    ['GET', '/api/actions?user_id=bad'], ['GET', '/api/registrations/lookup?email=a&other=b'],
    ['GET', '/api/actions/1?other=b'], ['DELETE', '/api/registrations'],
    ['POST', `/api/registrations/${'a'.repeat(64)}/balances`],
    ['GET', '/api/registrations/invalid/balances'],
    ['GET', `/api/registrations/${'a'.repeat(64)}/balances?address=injected`],
  ]) assert.equal(upstreamPath(method, url(path)), null, path);
  assert.equal(upstreamPath('GET', url('/api/registrations/lookup?email=alice%2Btest%40example.com')), '/v1/registrations/lookup?email=alice%2Btest%40example.com');
  assert.equal(upstreamPath('GET', url(`/api/actions?user_id=${'a'.repeat(64)}&limit=20&before_id=12`)), `/v1/actions?user_id=${'a'.repeat(64)}&limit=20&before_id=12`);
  assert.equal(upstreamPath('GET', url(`/api/registrations/${'a'.repeat(64)}/balances`)), null);
});

test('proxy keeps credentials server-side, validates origins and preserves backend results', async t => {
  const requests = [];
  const server = createServer(createHandler(config, (_req, res) => { res.writeHead(404); res.end(); }, async (url, options) => {
    requests.push({ url, options });
    return Response.json({ error: 'registration_not_found' }, { status: 404 });
  }));
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); server.close(); });
  const origin = `http://127.0.0.1:${server.address().port}`;
  const headers = { 'X-Hydro-Test': '1' };
  const response = await fetch(`${origin}/api/config`, { headers });
  assert.deepEqual(await response.json(), { inbox: config.inbox, chain: config.chain });
  assert.equal(requests.length, 0);
  for (const options of [{}, { headers: { ...headers, Origin: 'https://other.example' } }]) {
    assert.equal((await fetch(`${origin}/api/config`, options)).status, 403);
  }
  // Node fetch overrides Host, so use a raw HTTP request for this check.
  const wrongHostStatus = await new Promise((resolve, reject) => {
    const req = request(`${origin}/api/config`, { headers: { ...headers, Host: `other.example:${server.address().port}` } }, res => { res.resume(); resolve(res.statusCode); });
    req.on('error', reject); req.end();
  });
  assert.equal(wrongHostStatus, 403);
  const missing = await fetch(`${origin}/api/registrations/lookup?email=alice%2Btest%40example.com`, { headers });
  assert.equal(missing.status, 404);
  assert.equal(missing.headers.get('cache-control'), 'no-store');
  assert.deepEqual(await missing.json(), { error: 'registration_not_found' });
  assert.equal(requests[0].url, 'http://backend.invalid/v1/registrations/lookup?email=alice%2Btest%40example.com');
  assert.equal(requests[0].options.headers.Authorization, `Bearer ${config.apiKey}`);
  assert.equal(requests[0].options.redirect, 'error');
  const invalid = await fetch(`${origin}/api/registrations`, { method: 'POST', headers: { ...headers, 'Content-Type': 'application/json' }, body: '{"email":"test@example.com","proxy_address":"injected"}' });
  assert.equal(invalid.status, 400);
  assert.equal(requests.length, 1);
  const post = await fetch(`${origin}/api/registrations`, { method: 'POST', headers: { ...headers, 'Content-Type': 'application/json', Origin: origin }, body: '{"email":"test@example.com"}' });
  assert.equal(post.status, 404);
  assert.equal(requests[1].options.body, '{"email":"test@example.com"}');
  assert.equal((await fetch(`${origin}/api/actions`, { method: 'POST', headers })).status, 404);
  assert.equal(requests.length, 2);
});

test('upstream failures return a generic error without credential details', async t => {
  const server = createServer(createHandler(config, () => {}, async () => { throw new Error(config.apiKey); }));
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); server.close(); });
  const response = await fetch(`http://127.0.0.1:${server.address().port}/api/health`, { headers: { 'X-Hydro-Test': '1' } });
  assert.equal(response.status, 502);
  assert.deepEqual(await response.json(), { error: 'backend_unreachable' });
});
