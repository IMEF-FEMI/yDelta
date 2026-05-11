import { PublicKey } from '@solana/web3.js';
import { ProgramTestContext } from 'solana-bankrun';

/** Pyth PriceUpdateV2 receiver program. Marginfi's PythPushOracle adapter
 *  validates `oracle.owner == PYTH_RECEIVER_PROGRAM_ID`. */
export const PYTH_RECEIVER_PROGRAM_ID = new PublicKey('rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ');

const PRICE_UPDATE_V2_SIZE = 134;
/** yDelta's `decode_pyth_push` requires verification_level = Full (1) at offset 40. */
const PYTH_VERIFY_TAG_FULL = 1;

/** Overwrite the price/confidence/publish-time fields of a `PriceUpdateV2`
 *  account anchored to the current bankrun clock. When the account already
 *  exists (e.g. mainnet replay), we preserve its Anchor discriminator + feed
 *  id and only mutate the live fields; otherwise we synthesize a complete buf.
 *
 *  Field offsets within the account data (mirrors decode_pyth_push):
 *    0..8     discriminator (anchor)
 *    8..40    write_authority
 *    40       verification_level (1 = Full)
 *    41..73   feed_id
 *    73..81   price (i64)
 *    81..89   conf (u64)
 *    89..93   exponent (i32)
 *    93..101  publish_time (i64)
 *    101..109 prev_publish_time (i64)
 *    109..117 posted_slot (u64) */
export async function setPythPullOraclePrice({
  context,
  address,
  price,
  decimals,
  confidenceFraction = 0,
  slotOffsetSeconds = 0,
  feedId,
  owner = PYTH_RECEIVER_PROGRAM_ID,
  verificationLevel = PYTH_VERIFY_TAG_FULL,
  forceOwner = false,
}: {
  context: ProgramTestContext;
  address: PublicKey;
  price: number;
  decimals: number;
  /** Confidence relative to the price (e.g. 0.01 = 1%). Default 0. */
  confidenceFraction?: number;
  /** Negative offset to stale the publish_time for rejection tests. */
  slotOffsetSeconds?: number;
  feedId?: Buffer;
  owner?: PublicKey;
  /** Set to 0 to force the `OracleSetupUnsupported` path (Partial verification). */
  verificationLevel?: number;
  /** When true, overwrites the existing account's owner with `owner`. Use this
   *  when replaying mainnet state at a pubkey that was originally a non-Pyth
   *  oracle (e.g. SOL_ORACLE in our fixtures is a Switchboard account). */
  forceOwner?: boolean;
}): Promise<void> {
  const existing = await context.banksClient.getAccount(address);
  const reusable = existing && existing.data.length >= PRICE_UPDATE_V2_SIZE;
  const buf = reusable
    ? Buffer.from(existing!.data)
    : (() => {
        const b = Buffer.alloc(PRICE_UPDATE_V2_SIZE);
        // Synthetic anchor discriminator — any 8 bytes work (yDelta + marginfi
        // both gate on owner + buffer length, not the disc sighash).
        Buffer.from([1, 2, 3, 4, 5, 6, 7, 8]).copy(b, 0);
        return b;
      })();

  const scale = Math.pow(10, decimals);
  const mantissa = BigInt(Math.round(price * scale));
  const confidence = BigInt(Math.round(price * confidenceFraction * scale));
  const exponent = -decimals;
  const clock = await context.banksClient.getClock();
  const publishTime = clock.unixTimestamp + BigInt(slotOffsetSeconds);

  buf.writeUInt8(verificationLevel, 40);
  if (feedId) {
    if (feedId.length !== 32) throw new Error('feedId must be 32 bytes');
    feedId.copy(buf, 41);
  }
  buf.writeBigInt64LE(mantissa, 73);
  buf.writeBigUInt64LE(confidence, 81);
  buf.writeInt32LE(exponent, 89);
  buf.writeBigInt64LE(publishTime, 93);
  buf.writeBigInt64LE(publishTime - 1n, 101);
  buf.writeBigUInt64LE(clock.slot, 109);

  const useExistingOwner = existing && !forceOwner;
  void useExistingOwner;
  context.setAccount(address, {
    lamports: existing ? Number(existing.lamports) : 1_000_000_000,
    data: buf,
    owner: forceOwner || !existing ? owner : existing.owner,
    executable: false,
    rentEpoch: 0,
  });
}

/** Warp the bankrun clock forward by `seconds`. */
export async function warpClock(context: ProgramTestContext, seconds: number): Promise<void> {
  const { Clock } = await import('solana-bankrun');
  const clock = await context.banksClient.getClock();
  context.setClock(
    new Clock(
      clock.slot,
      clock.epochStartTimestamp,
      clock.epoch,
      clock.leaderScheduleEpoch,
      clock.unixTimestamp + BigInt(seconds),
    ),
  );
}
