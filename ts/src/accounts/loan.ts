/**
 * `LoanFixed` — 288-byte PDA at `[b"loan", market, sequence_le]`.
 *
 * The borrower owes `outstanding_debt_atoms` (accruing at `borrower_rate_bps`)
 * and the lender claims `lender_claimable_atoms` (accruing at `lender_rate_bps`).
 * Spread accrues onto a market-level fee accumulator. For P2Pool loans
 * (`loan_type == 1`), the borrower's marginfi liability is the live source of
 * truth — `outstanding_debt_atoms` is stale until `full_repay` retires it.
 */
import { PublicKey } from '@solana/web3.js';

import { LoanState, LoanType } from '../types.js';
import { readI64, readPubkey, readU128, readU16, readU64, readU8, readU32, view } from './_read.js';

export const LOAN_FIXED_SIZE = 288;
export const LOAN_FIXED_DISCRIMINANT = 0x79_64_65_6c_74_61_4c_4en; // "ydeltaLN"

export const LOAN_SECONDS_PER_YEAR = 31_536_000n;
export const LOAN_BPS_PER_UNIT = 10_000n;

export interface LoanFixed {
  market: PublicKey;
  createdBy: PublicKey;
  matchedLoanSequence: bigint;
  borrowerMarginfiBorrowShares: bigint;
  originationLtvBps: number;
  liquidationLtvBps: number;
  principalDebtAtoms: bigint;
  outstandingDebtAtoms: bigint;
  lenderClaimableAtoms: bigint;
  collateralAtoms: bigint;
  accumulatedProtocolFeeAtoms: bigint;
  accumulatedCuratorFeeAtoms: bigint;
  startedAtUnix: bigint;
  maturesAtUnix: bigint;
  lastAccruedUnix: bigint;
  lenderSeatIndex: number;
  borrowerSeatIndex: number;
  borrowerRateBps: number;
  lenderRateBps: number;
  state: LoanState;
  loanType: LoanType;
  flags: number;
  version: number;
  bump: number;
  lenderKind: number;
  lenderSubVaultId: number;
  curatorFeeBpsSnapshot: number;
  lenderGlobalVault: PublicKey;
  principalRetiredAtoms: bigint;
  cumulativeLenderGrossInterestAtoms: bigint;
  lenderDebtSharePriceSnapshotFp48: bigint;
  borrowerCollateralSharePriceSnapshotFp48: bigint;
}

export function decodeLoanFixed(data: Uint8Array | Buffer): LoanFixed {
  if (data.byteLength < LOAN_FIXED_SIZE) {
    throw new RangeError(
      `decodeLoanFixed: account too small (${data.byteLength} < ${LOAN_FIXED_SIZE})`,
    );
  }
  const dv = view(data);
  const disc = dv.getBigUint64(0, true);
  if (disc !== LOAN_FIXED_DISCRIMINANT) {
    throw new Error(
      `decodeLoanFixed: bad discriminator 0x${disc.toString(16)} (expected 0x${LOAN_FIXED_DISCRIMINANT.toString(16)})`,
    );
  }
  return {
    market: readPubkey(dv, 8),
    createdBy: readPubkey(dv, 40),
    matchedLoanSequence: readU64(dv, 72),
    borrowerMarginfiBorrowShares: readU128(dv, 80),
    originationLtvBps: readU16(dv, 96),
    liquidationLtvBps: readU16(dv, 98),
    principalDebtAtoms: readU64(dv, 112),
    outstandingDebtAtoms: readU64(dv, 120),
    lenderClaimableAtoms: readU64(dv, 128),
    collateralAtoms: readU64(dv, 136),
    accumulatedProtocolFeeAtoms: readU64(dv, 144),
    accumulatedCuratorFeeAtoms: readU64(dv, 152),
    startedAtUnix: readI64(dv, 160),
    maturesAtUnix: readI64(dv, 168),
    lastAccruedUnix: readI64(dv, 176),
    lenderSeatIndex: readU32(dv, 184),
    borrowerSeatIndex: readU32(dv, 188),
    borrowerRateBps: readU16(dv, 192),
    lenderRateBps: readU16(dv, 194),
    state: readU8(dv, 196) as LoanState,
    loanType: readU8(dv, 197) as LoanType,
    flags: readU8(dv, 198),
    version: readU8(dv, 199),
    bump: readU8(dv, 200),
    lenderKind: readU8(dv, 201),
    lenderSubVaultId: readU16(dv, 202),
    curatorFeeBpsSnapshot: readU16(dv, 204),
    lenderGlobalVault: readPubkey(dv, 208),
    principalRetiredAtoms: readU64(dv, 240),
    cumulativeLenderGrossInterestAtoms: readU64(dv, 248),
    lenderDebtSharePriceSnapshotFp48: readU128(dv, 256),
    borrowerCollateralSharePriceSnapshotFp48: readU128(dv, 272),
  };
}

/**
 * Project the loan's current `outstanding_debt_atoms` at unix time `now`.
 * Mirrors the on-chain `accrue_loan` formula for Fixed loans — P2Pool loans
 * should read the live marginfi liability instead.
 *
 *   elapsed = now - last_accrued
 *   new_debt = principal × borrower_rate_bps × elapsed / SECONDS_PER_YEAR / 10_000
 *   outstanding' = outstanding + new_debt
 */
export function projectOutstandingDebt(loan: LoanFixed, nowUnix: bigint): bigint {
  if (nowUnix <= loan.lastAccruedUnix) return loan.outstandingDebtAtoms;
  const elapsed = nowUnix - loan.lastAccruedUnix;
  const accrued =
    (loan.principalDebtAtoms * BigInt(loan.borrowerRateBps) * elapsed) /
    LOAN_SECONDS_PER_YEAR /
    LOAN_BPS_PER_UNIT;
  return loan.outstandingDebtAtoms + accrued;
}

/** Project the lender's current `lender_claimable_atoms` at unix time `now`. */
export function projectLenderClaimable(loan: LoanFixed, nowUnix: bigint): bigint {
  if (nowUnix <= loan.lastAccruedUnix) return loan.lenderClaimableAtoms;
  const elapsed = nowUnix - loan.lastAccruedUnix;
  const accrued =
    (loan.principalDebtAtoms * BigInt(loan.lenderRateBps) * elapsed) /
    LOAN_SECONDS_PER_YEAR /
    LOAN_BPS_PER_UNIT;
  return loan.lenderClaimableAtoms + accrued;
}
