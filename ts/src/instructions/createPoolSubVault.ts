import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 9 — `CreatePoolSubVault`. Protocol-admin-gated. Realloc-grows the
 * vault account for a fresh sub-vault block + inserts a `SubVault` tree
 * node of kind `Pool`. The `payer` (signer) must be
 * `GlobalConfig.protocolAdmin`; `curator` is stamped as the pool's curator
 * and `curatorFeeBps` (≤ `MAX_CURATOR_FEE_BPS`) is immutable after creation.
 *
 * `subVaultId` is **assigned by the program** from the vault's monotonic
 * `nextSubVaultId` counter (1-based; 0 is the sentinel/invalid). Callers
 * cannot request a specific id. The assigned id is reported back via
 * `SubVaultCreatedLog.subVaultId`.
 *
 * `maxLtvBps` is a fixed origination cap in `(0, 10_000)`;
 * `liquidationLtvBps` must satisfy `maxLtvBps + MIN_LIQ_GAP_BPS ≤ v ≤ 10_000`.
 */
export interface CreatePoolSubVaultArgs {
  payer: PublicKey;
  /** marginfi `Bank` (a market's debt lending pool) the vault is keyed to. */
  bank: PublicKey;
  curator: PublicKey;
  /** Quoted spread over the live marginfi lending APR, in bps. */
  spreadBps: number;
  /** Origination LTV cap in bps; `(0, 10_000)`. */
  maxLtvBps: number;
  /** Liquidation trigger in bps; `maxLtvBps + MIN_LIQ_GAP_BPS ≤ v ≤ 10_000`. */
  liquidationLtvBps: number;
  maxTermSeconds: number;
  /** Curator management fee in bps; `≤ MAX_CURATOR_FEE_BPS`. Immutable. */
  curatorFeeBps: number;
}

export function createPoolSubVaultInstruction(
  args: CreatePoolSubVaultArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.bank)[0];
  const data = new Writer()
    .u8(InstructionTag.CreatePoolSubVault)
    .pubkey(args.curator)
    .u16(args.spreadBps)
    .u16(args.maxLtvBps)
    .u16(args.liquidationLtvBps)
    .u32(args.maxTermSeconds)
    .u16(args.curatorFeeBps)
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
