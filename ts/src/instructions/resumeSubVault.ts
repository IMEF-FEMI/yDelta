import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 39 — `ResumeSubVault`. Vault-admin-gated. Flips `subVault.isSunset = 0`,
 * reversing `sunsetSubVaultInstruction` (tag 38). Restores deposits / new
 * orders / order updates / matches for the sub-vault.
 *
 * Admin escape hatch — there is no time lock on sunset, so an admin can
 * un-sunset a sub-vault freely.
 */
export interface ResumeSubVaultArgs {
  payer: PublicKey;
  /** marginfi `Bank` (a market's debt lending pool) the vault is keyed to. */
  bank: PublicKey;
  subVaultId: number;
}

export function resumeSubVaultInstruction(
  args: ResumeSubVaultArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.bank)[0];
  const data = new Writer()
    .u8(InstructionTag.ResumeSubVault)
    .u16(args.subVaultId)
    .toBuffer();
  return ydeltaIx(
    [signerRw(args.payer), ro(globalConfigPda()[0]), rw(vault)],
    data,
  );
}
