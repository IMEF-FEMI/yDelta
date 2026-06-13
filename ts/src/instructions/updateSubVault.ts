import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 32 — `UpdateSubVault`. Curator-gated (the owner, for Private
 * sub-vaults). `Option<None>` leaves a field unchanged; otherwise the new
 * value overrides. Rejected on sunset sub-vaults. The resulting LTV pair
 * must satisfy the liq-gap invariant. Match engine reads `maxLtvBps` live,
 * so a change here takes effect on the next match without seat re-sync.
 */
export interface UpdateSubVaultArgs {
  payer: PublicKey;
  /** marginfi `Bank` (a market's debt lending pool) the vault is keyed to. */
  bank: PublicKey;
  subVaultId: number;
  newMaxLtvBps?: number | null;
  newLiquidationLtvBps?: number | null;
  newMaxTermSeconds?: number | null;
  newSpreadBps?: number | null;
}

export function updateSubVaultInstruction(
  args: UpdateSubVaultArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.bank)[0];
  const data = new Writer()
    .u8(InstructionTag.UpdateSubVault)
    .u16(args.subVaultId)
    .optionU16(args.newMaxLtvBps ?? null)
    .optionU16(args.newLiquidationLtvBps ?? null)
    .optionU32(args.newMaxTermSeconds ?? null)
    .optionU16(args.newSpreadBps ?? null)
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
