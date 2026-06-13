import { PublicKey, TransactionInstruction } from '@solana/web3.js';
import BN from 'bn.js';

import { globalConfigPda, userAccountPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 42 — `CancelOrder` (v1 D6). Borrower-signed cancel of a resting bid,
 * releasing its encumbered collateral. Removes the market-side `RestingOrder`
 * + the borrower's `UserOrderRef`.
 */
export interface CancelOrderArgs {
  payer: PublicKey;
  market: PublicKey;
  orderSequence: bigint | BN | number;
  seatIndexHint?: number | null;
}

export function cancelOrderInstruction(args: CancelOrderArgs): TransactionInstruction {
  const data = new Writer()
    .u8(InstructionTag.CancelOrder)
    .u64(args.orderSequence)
    .optionDataIndex(args.seatIndexHint ?? null)
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
