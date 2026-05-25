import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 39 — `ResumeRiskProfile`. Vault-admin-gated. Flips
 * `profile.is_sunset = 0`, reversing `sunsetRiskProfileInstruction`
 * (tag 38). Restores deposits / new orders / order updates / matches
 * for the profile.
 *
 * Admin escape hatch — there is no time lock on sunset, so an admin can
 * un-sunset a profile freely.
 */
export interface ResumeRiskProfileArgs {
  payer: PublicKey;
  mint: PublicKey;
  profileId: number;
}

export function resumeRiskProfileInstruction(
  args: ResumeRiskProfileArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.ResumeRiskProfile)
    .u8(args.profileId)
    .toBuffer();
  return ydeltaIx(
    [signerRw(args.payer), ro(globalConfigPda()[0]), rw(vault)],
    data,
  );
}
