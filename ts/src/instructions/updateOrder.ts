import { PublicKey, TransactionInstruction } from '@solana/web3.js';
import BN from 'bn.js';

import { globalConfigPda, userAccountPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 44 — `UpdateOrder` (v1 D6). Borrower-signed cancel-and-replace of a
 * resting bid: the rate / term / expiry change while principal and
 * collateral are untouched. The order sequence is renewed (back of
 * price-time priority).
 */
export interface UpdateOrderArgs {
  payer: PublicKey;
  market: PublicKey;
  orderSequence: bigint | BN | number;
  newRateBps: number;
  newTermSeconds: number;
  newLastValidUnixTs: bigint | BN | number;
  seatIndexHint?: number | null;
  /**
   * New borrower LTV buffer (bps, 0..=10_000). Persists on the rested bid so a
   * later ask / MatchCrank cross honors the updated `effective_cap`. Defaults
   * to `0` (no buffer) when omitted.
   */
  newLtvBufferBps?: number;
}

export function updateOrderInstruction(args: UpdateOrderArgs): TransactionInstruction {
  const data = new Writer()
    .u8(InstructionTag.UpdateOrder)
    .u64(args.orderSequence)
    .optionDataIndex(args.seatIndexHint ?? null)
    .u16(args.newRateBps)
    .u32(args.newTermSeconds)
    .i64(args.newLastValidUnixTs)
    .u16(args.newLtvBufferBps ?? 0)
    .toBuffer();

  return ydeltaIx(
    [
      signerRw(args.payer),
      ro(globalConfigPda()[0]),
      rw(args.market),
      rw(userAccountPda(args.payer)[0]),
    ],
    data,
  );
}
