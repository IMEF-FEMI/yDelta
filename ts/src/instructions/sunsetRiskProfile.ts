import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 38 — `SunsetRiskProfile`. Vault-admin-gated. Flips
 * `profile.is_sunset = 1`, beginning the wind-down lifecycle:
 *   - deposits, new orders, order updates, and matches all reject with
 *     `VaultProfileSunset` (code 54)
 *   - withdrawals, curator-fee claims, cancels, repay, settle, and
 *     liquidate continue to operate
 *
 * Reversible via `resumeRiskProfileInstruction` (tag 39).
 * `removeRiskProfileInstruction` (tag 37) and
 * `adminCancelRiskProfileOrderInstruction` (tag 40) both gate on this flag.
 */
export interface SunsetRiskProfileArgs {
  payer: PublicKey;
  mint: PublicKey;
  profileId: number;
}

export function sunsetRiskProfileInstruction(
  args: SunsetRiskProfileArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.SunsetRiskProfile)
    .u8(args.profileId)
    .toBuffer();
  return ydeltaIx(
    [signerRw(args.payer), ro(globalConfigPda()[0]), rw(vault)],
    data,
  );
}
