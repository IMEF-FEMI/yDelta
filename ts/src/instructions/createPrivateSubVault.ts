import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 41 — `CreatePrivateSubVault`. Permissionless. Realloc-grows the vault
 * account for a fresh sub-vault block + inserts a `SubVault` tree node of
 * kind `Private`. The `payer` (signer) becomes the curator / sole depositor
 * and the curator fee is forced to 0.
 *
 * `subVaultId` is **assigned by the program** from the vault's monotonic
 * `nextSubVaultId` counter (1-based; 0 is the sentinel/invalid). Callers
 * cannot request a specific id. The assigned id is reported back via
 * `SubVaultCreatedLog.subVaultId`.
 *
 * `maxLtvBps` is a fixed origination cap in `(0, 10_000)`;
 * `liquidationLtvBps` must satisfy `maxLtvBps + MIN_LIQ_GAP_BPS ≤ v ≤ 10_000`.
 */
export interface CreatePrivateSubVaultArgs {
  payer: PublicKey;
  /** marginfi `Bank` (a market's debt lending pool) the vault is keyed to. */
  bank: PublicKey;
  /** Quoted spread over the live marginfi lending APR, in bps. */
  spreadBps: number;
  /** Origination LTV cap in bps; `(0, 10_000)`. */
  maxLtvBps: number;
  /** Liquidation trigger in bps; `maxLtvBps + MIN_LIQ_GAP_BPS ≤ v ≤ 10_000`. */
  liquidationLtvBps: number;
  maxTermSeconds: number;
}

export function createPrivateSubVaultInstruction(
  args: CreatePrivateSubVaultArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.bank)[0];
  const data = new Writer()
    .u8(InstructionTag.CreatePrivateSubVault)
    .u16(args.spreadBps)
    .u16(args.maxLtvBps)
    .u16(args.liquidationLtvBps)
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
