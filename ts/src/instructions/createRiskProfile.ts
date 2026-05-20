import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 9 — `CreateRiskProfile`. Vault-admin-gated. Realloc-grows the vault
 * account for a fresh 512-byte profile block + inserts a `RiskProfile` tree
 * node. `profile_id` is the new profile's index (0-indexed; max 255).
 */
export interface CreateRiskProfileArgs {
  payer: PublicKey;
  mint: PublicKey;
  profileId: number;
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
    .u8(args.profileId)
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
