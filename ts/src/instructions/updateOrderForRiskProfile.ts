import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signer, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 14 — `UpdateOrderForRiskProfile`. Zero-CPI cancel-and-replace. The
 * order sequence is renewed (back of price-time priority); rate/term/flags
 * can all change.
 */
export interface UpdateOrderForRiskProfileArgs {
  feePayer: PublicKey;
  curator: PublicKey;
  mint: PublicKey;
  market: PublicKey;
  profileId: number;
  newRateBps: number;
  newTermSeconds: number;
  newFlags?: number;
}

export function updateOrderForRiskProfileInstruction(
  args: UpdateOrderForRiskProfileArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.UpdateOrderForRiskProfile)
    .u8(args.profileId)
    .u16(args.newRateBps)
    .u32(args.newTermSeconds)
    .u8(args.newFlags ?? 0)
    .toBuffer();
  return ydeltaIx(
    [
      signerRw(args.feePayer),
      signer(args.curator),
      ro(globalConfigPda()[0]),
      rw(vault),
      rw(args.market),
      ro(SystemProgram.programId),
    ],
    data,
  );
}
