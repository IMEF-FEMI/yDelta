import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 37 — `RemoveRiskProfile`. Vault-admin-gated. Tears down the
 * `RiskProfile` keyed by `profileId` from the vault's `risk_profiles`
 * tree.
 *
 * **Hard precondition** (enforced on-chain): the profile must be empty.
 * Every balance field — `total_shares`, `total_assets_atoms`,
 * `total_principal_atoms`, `deployed_principal_atoms`,
 * `encumbered_in_orders_atoms`, `accumulated_curator_fee_atoms` — must
 * be zero or the ix rejects with `VaultProfileNotEmpty`.
 *
 * The freed 512-byte block returns to the vault's profile free list;
 * `risk_profile_count` decrements. The vault's `next_profile_id`
 * counter is **not** touched: once a `profile_id` has been used in a
 * vault it is never re-issued, so historical loan references stay
 * unambiguous.
 */
export interface RemoveRiskProfileArgs {
  payer: PublicKey;
  mint: PublicKey;
  profileId: number;
}

export function removeRiskProfileInstruction(
  args: RemoveRiskProfileArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.RemoveRiskProfile)
    .u8(args.profileId)
    .toBuffer();
  return ydeltaIx(
    [signerRw(args.payer), ro(globalConfigPda()[0]), rw(vault)],
    data,
  );
}
