import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signer, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 14 — `UpdateOrderForSubVault`. Curator-gated cancel-and-replace of a
 * vault ask in a single transaction with a fresh order sequence (back of
 * price-time priority). v1 D4: takes **no** rate or term — the processor
 * recomputes `live bank lending APR + subVault.spreadBps` and uses
 * `subVault.maxTermSeconds`; only `newFlags` changes the order bitmask.
 *
 * Same accounts as `placeOrderForSubVaultInstruction`: the vault PDA is
 * derived from `debtBank`, which (with `marginfiGroup`) must match the market
 * header so the processor can read the live bank APR.
 */
export interface UpdateOrderForSubVaultArgs {
  feePayer: PublicKey;
  curator: PublicKey;
  market: PublicKey;
  debtBank: PublicKey;
  marginfiGroup: PublicKey;
  collateralBank: PublicKey;
  debtOracles: PublicKey[];
  collateralOracles: PublicKey[];
  subVaultId: number;
  /** New order bitmask. Pass `0` unless you know otherwise. */
  newFlags?: number;
}

export function updateOrderForSubVaultInstruction(
  args: UpdateOrderForSubVaultArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.debtBank)[0];
  const data = new Writer()
    .u8(InstructionTag.UpdateOrderForSubVault)
    .u16(args.subVaultId)
    .u8(args.newFlags ?? 0)
    .toBuffer();
  return ydeltaIx(
    [
      signerRw(args.feePayer),
      signer(args.curator),
      ro(globalConfigPda()[0]),
      rw(vault),
      rw(args.market),
      ro(args.debtBank),
      ro(args.marginfiGroup),
      ro(args.collateralBank),
      ...args.debtOracles.map(ro),
      ...args.collateralOracles.map(ro),
      ro(SystemProgram.programId),
    ],
    data,
  );
}
