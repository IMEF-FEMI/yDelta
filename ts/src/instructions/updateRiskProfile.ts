import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 32 — `UpdateRiskProfile`. Vault-admin-gated. `Option<None>` leaves a
 * field unchanged; otherwise the new value overrides. Match engine reads
 * `max_ltv_bps` live, so a change here takes effect on the next match
 * without seat re-sync.
 */
export interface UpdateRiskProfileArgs {
  payer: PublicKey;
  mint: PublicKey;
  profileId: number;
  newMaxLtvBps?: number | null;
  newMaxTermSeconds?: number | null;
}

export function updateRiskProfileInstruction(
  args: UpdateRiskProfileArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.UpdateRiskProfile)
    .u8(args.profileId)
    .optionU16(args.newMaxLtvBps ?? null)
    .optionU32(args.newMaxTermSeconds ?? null)
    .toBuffer();
  return ydeltaIx(
    [
      signerRw(args.payer),
      ro(globalConfigPda()[0]),
      rw(vault),
      ro(SystemProgram.programId),
    ],
    data,
  );
}
