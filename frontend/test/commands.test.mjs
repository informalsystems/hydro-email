import test from 'node:test';
import assert from 'node:assert/strict';
import { buildSubject, mailto } from '../src/commands.mjs';

test('supported subjects preserve destination and denomination case', () => {
  assert.equal(buildSubject('deposit'), 'deposit');
  assert.equal(buildSubject('forward'), 'forward');
  assert.equal(buildSubject('withdraw', 'neutron1Address', 'ibc/ABC', '42'), 'withdraw neutron1Address ibc/ABC 42');
  assert.equal(buildSubject('withdraw_receipt', 'address', 'factory/vault/Share', '340282366920938463463374607431768211455'), 'withdraw_receipt address factory/vault/Share 340282366920938463463374607431768211455');
});

test('withdrawals reject malformed arguments and amounts outside Uint128', () => {
  for (const amount of ['', '0', '-1', '+1', '1.2', '1e6', '340282366920938463463374607431768211456']) {
    assert.throws(() => buildSubject('withdraw', 'address', 'denom', amount));
  }
  assert.throws(() => buildSubject('withdraw', 'address extra', 'denom', '1'));
  assert.throws(() => buildSubject('withdraw', 'address', 'denom\r\nextra', '1'));
  assert.throws(() => buildSubject('withdraw_receipt', '', 'denom', '1'));
  assert.throws(() => buildSubject('other'));
});

test('mailto encodes the recipient and exact subject without injecting headers', () => {
  const subject = buildSubject('withdraw', 'address', 'ibc/ABC', '12');
  const link = mailto('inbox+test@example.com', subject);
  assert.equal(link, 'mailto:inbox%2Btest%40example.com?subject=withdraw%20address%20ibc%2FABC%2012');
  assert.equal(new URL(link).searchParams.get('subject'), subject);
  for (const inbox of ['', 'inbox@example.com?bcc=other@example.com', 'inbox@example.com\r\nBcc:test']) {
    assert.throws(() => mailto(inbox, 'deposit'));
  }
});
