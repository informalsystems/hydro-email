const MAX_AMOUNT = 340282366920938463463374607431768211455n;

export function buildSubject(command, address = '', denom = '', amount = '') {
  if (command === 'deposit' || command === 'forward') return command;
  if (!['withdraw', 'withdraw_receipt'].includes(command)) throw new Error('Choose a supported action.');
  if (!address || /\s/.test(address)) throw new Error('Enter a destination address without spaces.');
  if (!denom || /\s/.test(denom)) throw new Error('Enter a token denomination without spaces.');
  if (!/^[0-9]+$/.test(amount) || amount.length > 39 || BigInt(amount) === 0n || BigInt(amount) > MAX_AMOUNT) {
    throw new Error('Amount must be a positive integer in base units, within Uint128.');
  }
  return `${command} ${address} ${denom} ${amount}`;
}

export function mailto(inbox, subject) {
  if (!/^[^\s@<>,;?&#]+@[^\s@<>,;?&#]+$/.test(inbox)) throw new Error('Configure a valid recipient in WAVS_ENV_IMAP_USERNAME.');
  return `mailto:${encodeURIComponent(inbox)}?subject=${encodeURIComponent(subject)}`;
}
