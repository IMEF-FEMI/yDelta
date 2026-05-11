import { expect } from 'chai';
import BN from 'bn.js';
import { BanksTransactionResultWithMeta } from 'solana-bankrun';
import { decodeError, findYdeltaErrorCodeInLogs } from '../../../src/helpers/errors';

export function assertBNApprox(actual: BN, expected: BN, toleranceAbs: BN | number = 0): void {
  const tol = BN.isBN(toleranceAbs) ? toleranceAbs : new BN(toleranceAbs);
  const diff = actual.sub(expected).abs();
  if (diff.gt(tol)) {
    throw new Error(`expected ${expected.toString()} ≈ ${actual.toString()} (tol ${tol.toString()})`);
  }
}

export function assertBNEqual(actual: BN, expected: BN | number, msg?: string): void {
  const e = BN.isBN(expected) ? expected : new BN(expected);
  if (!actual.eq(e)) {
    throw new Error(`${msg ?? 'BN mismatch'}: expected ${e.toString()}, got ${actual.toString()}`);
  }
}

/** Assert that a bankrun tx result failed with a specific `YdeltaError`. */
export function expectYdeltaError(
  result: BanksTransactionResultWithMeta,
  errorName: string,
): void {
  if (!result.result) {
    expect.fail(`expected ${errorName} but tx succeeded`);
  }
  const logs = (result.meta?.logMessages ?? []) as string[];
  const code = findYdeltaErrorCodeInLogs(logs);
  if (code === null) {
    expect.fail(
      `expected ${errorName}, no ydelta Custom code in logs:\n${logs.join('\n')}`,
    );
  }
  const decoded = decodeError({ code });
  if (!decoded) {
    expect.fail(`expected ${errorName}, got unknown code ${code}`);
  }
  expect(decoded.name).to.equal(errorName);
}

/** Async wrapper: invoke `fn()`, capture the bankrun result, assert
 *  `YdeltaError(errorName)` was emitted. */
export async function expectFailedTxWithError(
  fn: () => Promise<BanksTransactionResultWithMeta>,
  errorName: string,
): Promise<void> {
  const result = await fn();
  expectYdeltaError(result, errorName);
}
