// Oracle staleness probes. Provider-specific refresh-ix builders are out of
// scope here — consumers should drive Pyth-receiver / Switchboard-on-demand
// SDKs and prepend the result via `maybePrependOracleRefresh`.
import { Connection, PublicKey, TransactionInstruction } from '@solana/web3.js';

import { Bank, OracleSetup } from './marginfi.js';

export interface PriceUpdateLiveness {
  lastUpdateSlot: bigint;
  currentSlot: bigint;
  slotAge: bigint;
}

// PriceUpdateV2 byte map (post-anchor):
//   @8   verification_level    u8
//   @9   write_authority_inner u8
//   @10  write_authority       Pubkey
//   @42  posted_slot           u64
export function readPythPostedSlot(accountData: Uint8Array | Buffer): bigint {
  if (accountData.byteLength < 8 + 50) {
    throw new RangeError(`readPythPostedSlot: account too small (${accountData.byteLength})`);
  }
  const dv = new DataView(
    accountData.buffer,
    accountData.byteOffset,
    accountData.byteLength,
  );
  return dv.getBigUint64(8 + 42, true);
}

// 2.5 slots/sec is the conservative mainnet rate.
export function isPriceUpdateStale(
  liveness: PriceUpdateLiveness,
  maxAgeSeconds: number,
  slotsPerSecond: number = 2.5,
): boolean {
  const maxAgeSlots = BigInt(Math.ceil(maxAgeSeconds * slotsPerSecond));
  return liveness.slotAge > maxAgeSlots;
}

export async function probeOracleLiveness(
  conn: Connection,
  oracle: PublicKey,
  setup: OracleSetup,
): Promise<PriceUpdateLiveness | null> {
  const acc = await conn.getAccountInfo(oracle);
  if (!acc) return null;
  const currentSlot = BigInt(await conn.getSlot());
  let lastUpdateSlot: bigint;
  switch (setup) {
    case OracleSetup.PythPushOracle:
    case OracleSetup.StakedWithPythPush:
      lastUpdateSlot = readPythPostedSlot(acc.data);
      break;
    case OracleSetup.SwitchboardPull:
      // Switchboard slot is deep in PullFeedAccountData; rely on the
      // provider SDK for a precise read.
      return null;
    default:
      return null;
  }
  return { lastUpdateSlot, currentSlot, slotAge: currentSlot - lastUpdateSlot };
}

export async function bankNeedsOracleRefresh(
  conn: Connection,
  bank: Bank,
): Promise<boolean | null> {
  const live = await probeOracleLiveness(conn, bank.oracleKeys[0], bank.oracleSetup);
  if (live === null) return null;
  return isPriceUpdateStale(live, bank.oracleMaxAgeSeconds);
}

export function maybePrependOracleRefresh(
  refreshIxs: TransactionInstruction[] | undefined,
  yDeltaIxs: TransactionInstruction[],
): TransactionInstruction[] {
  if (!refreshIxs || refreshIxs.length === 0) return yDeltaIxs;
  return [...refreshIxs, ...yDeltaIxs];
}
