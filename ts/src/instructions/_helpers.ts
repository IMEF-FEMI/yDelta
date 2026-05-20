import {
  AccountMeta,
  ComputeBudgetProgram,
  PublicKey,
  TransactionInstruction,
} from '@solana/web3.js';

import { HEAVY_IX_CU_LIMIT, YDELTA_PROGRAM_ID } from '../constants.js';

/** Read-only, non-signer account meta. */
export function ro(pubkey: PublicKey): AccountMeta {
  return { pubkey, isSigner: false, isWritable: false };
}

/** Writable, non-signer account meta. */
export function rw(pubkey: PublicKey): AccountMeta {
  return { pubkey, isSigner: false, isWritable: true };
}

/** Read-only signer. */
export function signer(pubkey: PublicKey): AccountMeta {
  return { pubkey, isSigner: true, isWritable: false };
}

/** Writable signer (the fee payer / state-mutating signer). */
export function signerRw(pubkey: PublicKey): AccountMeta {
  return { pubkey, isSigner: true, isWritable: true };
}

/**
 * Assemble a yDelta TransactionInstruction. Centralised so the program id
 * is sourced from `constants.YDELTA_PROGRAM_ID` and never re-declared.
 */
export function ydeltaIx(
  keys: AccountMeta[],
  data: Buffer,
): TransactionInstruction {
  return new TransactionInstruction({
    programId: YDELTA_PROGRAM_ID,
    keys,
    data,
  });
}

/**
 * Heavy ixs (PlaceOrder, ProcessMatchedLoan, LiquidateLoan, SettleMaturedLoan,
 * ClaimRepaymentForRiskProfile) need the BPF CU limit raised. Callers compose
 * the budget ix at the head of their tx — this helper returns it precomputed.
 */
export function cuBudgetIx(limit: number = HEAVY_IX_CU_LIMIT): TransactionInstruction {
  return ComputeBudgetProgram.setComputeUnitLimit({ units: limit });
}

/**
 * Prepend the CU-budget ix to `ixs`. Most callers will assemble heavier txs
 * inline; this is the one-shot helper for "wrap this single heavy ix".
 */
export function withCuBudget(
  ix: TransactionInstruction | TransactionInstruction[],
  limit: number = HEAVY_IX_CU_LIMIT,
): TransactionInstruction[] {
  const arr = Array.isArray(ix) ? ix : [ix];
  return [cuBudgetIx(limit), ...arr];
}
