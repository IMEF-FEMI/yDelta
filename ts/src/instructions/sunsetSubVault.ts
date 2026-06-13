import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 38 — `SunsetSubVault`. Vault-admin-gated. Flips `subVault.isSunset = 1`,
 * beginning the wind-down lifecycle:
 *   - deposits, new orders, order updates, and matches all reject with
 *     `SubVaultSunset` (code 54)
 *   - withdrawals, curator-fee claims, cancels, repay, settle, and
 *     liquidate continue to operate
 *
 * Reversible via `resumeSubVaultInstruction` (tag 39).
 * `removeSubVaultInstruction` (tag 37) and
 * `adminCancelSubVaultOrderInstruction` (tag 40) both gate on this flag.
 */
export interface SunsetSubVaultArgs {
  payer: PublicKey;
  /** marginfi `Bank` (a market's debt lending pool) the vault is keyed to. */
  bank: PublicKey;
  subVaultId: number;
}

export function sunsetSubVaultInstruction(
  args: SunsetSubVaultArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.bank)[0];
  const data = new Writer()
    .u8(InstructionTag.SunsetSubVault)
    .u16(args.subVaultId)
    .toBuffer();
  return ydeltaIx(
    [signerRw(args.payer), ro(globalConfigPda()[0]), rw(vault)],
    data,
  );
}
