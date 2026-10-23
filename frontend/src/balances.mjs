// Avoid Number: chain balances can exceed JavaScript's safe integer range.
export function formatAtom(amount) {
  if (!/^[0-9]+$/.test(amount)) throw new Error('Invalid uatom balance');
  const units = BigInt(amount);
  const whole = units / 1000000n;
  const fraction = (units % 1000000n).toString().padStart(6, '0').replace(/0+$/, '');
  return fraction ? `${whole}.${fraction}` : whole.toString();
}

// Public Cosmos Hub REST endpoint listed in cosmos/chain-registry.
const COSMOS_HUB_REST = 'https://cosmos-rest.publicnode.com';

export async function fetchBalances(address, fetcher = fetch) {
  if (!/^cosmos1[0-9a-z]{20,100}$/.test(address)) {
    throw new Error('Balance lookup requires a Cosmos Hub address.');
  }
  const balances = [];
  const seen = new Set();
  let cursor = '';
  const signal = AbortSignal.timeout(10000);
  do {
    const url = new URL(`${COSMOS_HUB_REST}/cosmos/bank/v1beta1/balances/${address}`);
    url.searchParams.set('pagination.limit', '100');
    if (cursor) url.searchParams.set('pagination.key', cursor);
    const response = await fetcher(url, { signal, credentials: 'omit', cache: 'no-store' });
    if (!response.ok) throw new Error(`Cosmos Hub balance query failed (${response.status}).`);
    const page = await response.json();
    if (!Array.isArray(page.balances) || page.balances.some(coin =>
      typeof coin.denom !== 'string' || typeof coin.amount !== 'string' || !/^[0-9]+$/.test(coin.amount))) {
      throw new Error('Invalid Cosmos Hub balance response.');
    }
    balances.push(...page.balances);
    cursor = page.pagination?.next_key ?? '';
    if (typeof cursor !== 'string' || (cursor && seen.has(cursor))) throw new Error('Invalid balance pagination.');
    seen.add(cursor);
  } while (cursor);
  return balances;
}
