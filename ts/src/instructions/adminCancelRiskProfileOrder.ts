import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 40 — `AdminCancelRiskProfileOrder`. Vault-admin-gated force cancel
 * of a sunset profile's resting market ask. Rejects with
 * `VaultProfileNotSunset` (code 55) unless `profile.is_sunset == 1`.
 *
 * Non-sunset profiles must use the curator-gated
 * `cancelOrderForRiskProfileInstruction` (tag 13) instead — this is the
 * wind-down escape hatch for orders whose curator key is unavailable.
 *
 * Removes both the market-side resting ask and the vault-side
 * `RiskProfileOrderRef`; emits `CancelOrderForRiskProfileLog`.
 */
export interface AdminCancelRiskProfileOrderArgs {
  payer: PublicKey;
  mint: PublicKey;
  market: PublicKey;
  profileId: number;
}

export function adminCancelRiskProfileOrderInstruction(
  args: AdminCancelRiskProfileOrderArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.AdminCancelRiskProfileOrder)
    .u8(args.profileId)
    .toBuffer();
  return ydeltaIx(
    [
      signerRw(args.payer),
      ro(globalConfigPda()[0]),
      rw(vault),
      rw(args.market),
    ],
    data,
  );
}
