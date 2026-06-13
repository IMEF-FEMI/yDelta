import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signer, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 12 — `PlaceOrderForSubVault`. Curator rests an unbounded vault ask on a
 * market, auto-creating the per-`(subVaultId, market)` `ClaimedSeat` and
 * `SubVaultOrderRef` on first call. v1 D4: takes **no** rate or term — the
 * processor computes `live bank lending APR + subVault.spreadBps` and uses
 * `subVault.maxTermSeconds`; `flags` carries the order bitmask.
 *
 * Uses the **split-payer** pattern: `feePayer` covers tx fees + any realloc
 * rent; `curator` is the verifying signer (must equal `subVault.curator`).
 * Pass the same key for both when they're the same wallet.
 *
 * The vault PDA is derived from `debtBank` (the market's debt lending pool);
 * `debtBank` + `marginfiGroup` must match the market header so the processor
 * can read the live bank APR.
 */
export interface PlaceOrderForSubVaultArgs {
  feePayer: PublicKey;
  curator: PublicKey;
  market: PublicKey;
  debtBank: PublicKey;
  marginfiGroup: PublicKey;
  collateralBank: PublicKey;
  debtOracles: PublicKey[];
  collateralOracles: PublicKey[];
  subVaultId: number;
  /** Order bitmask. Pass `0` unless you know otherwise. */
  flags?: number;
}

export function placeOrderForSubVaultInstruction(
  args: PlaceOrderForSubVaultArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.debtBank)[0];
  const data = new Writer()
    .u8(InstructionTag.PlaceOrderForSubVault)
    .u16(args.subVaultId)
    .u8(args.flags ?? 0)
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
