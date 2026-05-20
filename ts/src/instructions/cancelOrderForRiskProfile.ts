import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signer, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 13 — `CancelOrderForRiskProfile`. Zero-CPI. Removes the
 * market-side `RestingOrder` + vault-side `RiskProfileOrderRef`. Idempotent
 * on missing (no error if there's nothing to cancel).
 */
export function cancelOrderForRiskProfileInstruction(args: {
  feePayer: PublicKey;
  curator: PublicKey;
  mint: PublicKey;
  market: PublicKey;
  profileId: number;
}): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.CancelOrderForRiskProfile)
    .u8(args.profileId)
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
