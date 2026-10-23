import test from 'node:test';
import assert from 'node:assert/strict';
import { formatAtom } from '../src/balances.mjs';

test('uatom formatting is exact for zero, fractional ATOM, and amounts beyond Number precision', () => {
  for (const [base, display] of [
    ['0', '0'], ['1', '0.000001'], ['100000', '0.1'], ['1000000', '1'],
    ['1234567', '1.234567'], ['340282366920938463463374607431768211455', '340282366920938463463374607431768.211455'],
  ]) assert.equal(formatAtom(base), display);
  for (const invalid of ['-1', '1.5', 'unknown']) assert.throws(() => formatAtom(invalid));
});

test('balance queries go directly to Cosmos Hub, paginate and send no credentials', async () => {
  const { fetchBalances } = await import('../src/balances.mjs');
  const calls = [];
  const address = `cosmos1${'a'.repeat(38)}`;
  const expected = [{ denom: 'uatom', amount: '1234567' }, { denom: 'factory/vault/shares', amount: '340282366920938463463374607431768211455' }];
  const balances = await fetchBalances(address, async (url, options) => {
    calls.push({ url, options });
    return Response.json(calls.length === 1
      ? { balances: [expected[0]], pagination: { next_key: '+/cursor=' } }
      : { balances: [expected[1]], pagination: { next_key: null } });
  });
  assert.deepEqual(balances, expected);
  assert.equal(calls.length, 2);
  assert.equal(calls[0].url.origin, 'https://cosmos-rest.publicnode.com');
  assert.equal(calls[0].url.pathname, `/cosmos/bank/v1beta1/balances/${address}`);
  assert.equal(calls[1].url.searchParams.get('pagination.key'), '+/cursor=');
  assert.equal(calls[0].options.credentials, 'omit');
  assert.equal(calls[0].options.headers, undefined);
});

test('only a successful empty response means zero; query failures remain errors', async () => {
  const { fetchBalances } = await import('../src/balances.mjs');
  const address = `cosmos1${'a'.repeat(38)}`;
  assert.deepEqual(await fetchBalances(address, async () => Response.json({ balances: [] })), []);
  await assert.rejects(fetchBalances('neutron1wrongchain'), /Cosmos Hub address/);
  await assert.rejects(fetchBalances(address, async () => new Response('', { status: 503 })), /query failed/);
  await assert.rejects(fetchBalances(address, async () => Response.json({})), /Invalid/);
  await assert.rejects(fetchBalances(address, async () => Response.json({ balances: [{ denom: 'uatom', amount: 9007199254740992 }] })), /Invalid/);
  await assert.rejects(fetchBalances(address, async () => Response.json({ balances: [], pagination: { next_key: 'repeated' } })), /pagination/);
});
