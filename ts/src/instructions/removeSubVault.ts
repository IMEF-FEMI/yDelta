import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 37 — `RemoveSubVault`. Vault-admin-gated. Tears down the `SubVault`
 * keyed by `subVaultId` from the vault's sub-vaults tree. The target must
 * already have `isSunset == 1`.
 *
 * **Hard precondition** (enforced on-chain): the sub-vault must be empty.
 * Every balance field — `totalShares`, `totalAssetsAtoms`,
 * `totalPrincipalAtoms`, `deployedPrincipalAtoms`,
 * `encumberedInOrdersAtoms`, `accumulatedCuratorFeeAtoms` — must be zero or
 * the ix rejects with `SubVaultNotEmpty`.
 *
 * The freed block returns to the vault's profile free list; `subVaultCount`
 * decrements. The vault's `nextSubVaultId` counter is **not** touched: once
 * a `subVaultId` has been used in a vault it is never re-issued, so
 * historical loan references stay unambiguous.
 */
export interface RemoveSubVaultArgs {
  payer: PublicKey;
  /** marginfi `Bank` (a market's debt lending pool) the vault is keyed to. */
  bank: PublicKey;
  subVaultId: number;
}

export function removeSubVaultInstruction(
  args: RemoveSubVaultArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.bank)[0];
  const data = new Writer()
    .u8(InstructionTag.RemoveSubVault)
    .u16(args.subVaultId)
    .toBuffer();
  return ydeltaIx(
    [signerRw(args.payer), ro(globalConfigPda()[0]), rw(vault)],
    data,
  );
}
