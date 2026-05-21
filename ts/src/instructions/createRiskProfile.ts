import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 9 — `CreateRiskProfile`. Vault-admin-gated. Realloc-grows the vault
 * account for a fresh 512-byte profile block + inserts a `RiskProfile` tree
 * node.
 *
 * `profile_id` is **assigned by the program** from the vault's monotonic
 * `next_profile_id` counter — callers cannot request a specific id. The
 * assigned id is reported back via `RiskProfileCreatedLog.profile_id` and
 * can be predicted as the vault's current `next_profile_id` value.
 */
export interface CreateRiskProfileArgs {
  payer: PublicKey;
  mint: PublicKey;
  curator: PublicKey;
  maxLtvBps: number;
  maxTermSeconds: number;
}

export function createRiskProfileInstruction(
  args: CreateRiskProfileArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.CreateRiskProfile)
    .pubkey(args.curator)
    .u16(args.maxLtvBps)
    .u32(args.maxTermSeconds)
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
