/**
 * Helpers for asserting transactions fail with specific on-chain error codes.
 * `bk.send()` throws on processor failure with the Solana error string format:
 *   "...custom program error: 0xN..."
 * where `N` is the hex `Custom(...)` code emitted by the program.
 */
import { expect } from 'vitest';

/** Custom error codes from `programs/ydelta/src/program/error.rs::YdeltaError`. */
export const YdeltaError = {
  IncorrectAccount: 10,
  VaultProfileNotFound: 26,
  VaultCuratorRequired: 29,
  VaultAdminRequired: 30,
  MarketAdminRequired: 43,
  PendingAdminMismatch: 45,
  MarketPaused: 46,
  GlobalPaused: 47,
  ProtocolAdminRequired: 48,
  VaultPaused: 50,
} as const;

/**
 * Assert that `promise` rejects with a transaction that errored with the given
 * Solana `Custom(code)` value. Hex-matches the well-known Solana error format.
 */
export async function expectCustomError(
  promise: Promise<unknown>,
  code: number,
  label?: string,
): Promise<void> {
  let err: unknown;
  try {
    await promise;
    expect.fail(`expected Custom(${code})${label ? ` (${label})` : ''} but tx succeeded`);
  } catch (e) {
    err = e;
  }
  const msg = err instanceof Error ? err.message : String(err);
  const hex = `0x${code.toString(16)}`;
  expect(
    msg.includes(`custom program error: ${hex}`),
    `expected Custom(${code})=${hex}${label ? ` (${label})` : ''} in ${msg}`,
  ).toBe(true);
}
