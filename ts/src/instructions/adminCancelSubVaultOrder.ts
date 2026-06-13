import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 40 — `AdminCancelSubVaultOrder`. Vault-admin-gated force cancel of a
 * sunset sub-vault's resting market ask. Rejects with `SubVaultNotSunset`
 * (code 55) unless `subVault.isSunset == 1`.
 *
 * Non-sunset sub-vaults must use the curator-gated
 * `cancelOrderForSubVaultInstruction` (tag 13) instead — this is the
 * wind-down escape hatch for orders whose curator key is unavailable.
 *
 * Removes both the market-side resting ask and the vault-side
 * `SubVaultOrderRef`. The vault PDA is derived from `debtBank` (the market's
 * debt lending pool).
 */
export interface AdminCancelSubVaultOrderArgs {
  payer: PublicKey;
  debtBank: PublicKey;
  market: PublicKey;
  subVaultId: number;
}

export function adminCancelSubVaultOrderInstruction(
  args: AdminCancelSubVaultOrderArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.debtBank)[0];
  const data = new Writer()
    .u8(InstructionTag.AdminCancelSubVaultOrder)
    .u16(args.subVaultId)
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
