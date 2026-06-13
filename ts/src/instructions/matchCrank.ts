import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 43 — `MatchCrank` (v1 D7/D8). Permissionless crank that walks resting
 * asks against resting bids, filling up to `maxFills` crosses per call
 * (`0` is treated as `1`). Anyone may sign. The `vault` is derived from
 * `debtBank` (the market's debt lending pool). Heavy instruction — callers
 * SHOULD prepend `withCuBudget(...)`.
 */
export interface MatchCrankArgs {
  payer: PublicKey;
  market: PublicKey;
  debtBank: PublicKey;
  marginfiGroup: PublicKey;
  collateralBank: PublicKey;
  debtOracles: PublicKey[];
  collateralOracles: PublicKey[];
  maxFills: number;
}

export function matchCrankInstruction(args: MatchCrankArgs): TransactionInstruction {
  const vault = globalVaultPda(args.debtBank)[0];

  const data = new Writer()
    .u8(InstructionTag.MatchCrank)
    .u32(args.maxFills)
    .toBuffer();

  return ydeltaIx(
    [
      signerRw(args.payer),
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
