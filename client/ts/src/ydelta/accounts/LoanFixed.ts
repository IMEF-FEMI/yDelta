import { PublicKey } from '@solana/web3.js';
import BN from 'bn.js';
import {
  LOAN_FIXED_DISCRIMINANT,
  readU64Le,
} from '../../utils/discriminator';
import { LoanState, LoanType, LenderKind } from '../types/enums';

export const LOAN_FIXED_SIZE = 288;

export type LoanFixed = {
  discriminator: bigint;
  market: PublicKey;
  createdBy: PublicKey;
  matchedLoanSequence: BN;
  /** fp48 marginfi share quantity (raw I80F48 mantissa as u128) */
  borrowerMarginfiBorrowShares: BN;
  /** fp48 (raw I80F48 mantissa as u128) */
  debtIndexAtLastAccrual: BN;
  /** atoms */
  principalDebtAtoms: BN;
  /** atoms */
  outstandingDebtAtoms: BN;
  /** atoms */
  lenderClaimableAtoms: BN;
  /** atoms */
  collateralAtoms: BN;
  /** atoms */
  accumulatedProtocolFeeAtoms: BN;
  /** atoms */
  accumulatedCuratorFeeAtoms: BN;
  /** unix seconds */
  startedAtUnix: BN;
  /** unix seconds */
  maturesAtUnix: BN;
  /** unix seconds */
  lastAccruedUnix: BN;
  lenderSeatIndex: number;
  borrowerSeatIndex: number;
  /** bps */
  borrowerRateBps: number;
  /** bps */
  lenderRateBps: number;
  state: LoanState;
  loanType: LoanType;
  /** bit field: see LOAN_FLAG_* consts */
  flags: number;
  version: number;
  bump: number;
  lenderKind: LenderKind;
  /** 0 for wallet-funded loans */
  lenderProfileId: number;
  hasRestingSecondaryBid: boolean;
  /** bps */
  curatorFeeBpsSnapshot: number;
  lenderGlobalVault: PublicKey;
  /** atoms */
  principalRetiredAtoms: BN;
  /** fp48 marginfi share-price snapshot (raw I80F48 mantissa as u128) */
  lenderDebtSharePriceSnapshotFp48: BN;
  /** fp48 marginfi share-price snapshot (raw I80F48 mantissa as u128) */
  borrowerCollateralSharePriceSnapshotFp48: BN;
};

function readU128(buf: Buffer, offset: number): BN {
  return new BN(buf.subarray(offset, offset + 16), 'le');
}

function readI64(buf: Buffer, offset: number): BN {
  const u = new BN(buf.subarray(offset, offset + 8), 'le');
  // I64 interpretation: if high bit set, subtract 2^64.
  const TWO_64 = new BN(1).shln(64);
  if (u.testn(63)) return u.sub(TWO_64);
  return u;
}

export function decodeLoanFixed(buf: Buffer): LoanFixed {
  if (buf.length < LOAN_FIXED_SIZE) {
    throw new Error(`Loan buffer too small: ${buf.length} < ${LOAN_FIXED_SIZE}`);
  }
  const discriminator = readU64Le(buf, 0);
  if (discriminator !== LOAN_FIXED_DISCRIMINANT) {
    throw new Error(
      `Loan discriminator mismatch: 0x${discriminator.toString(16)} != 0x${LOAN_FIXED_DISCRIMINANT.toString(16)}`,
    );
  }
  return {
    discriminator,
    market: new PublicKey(buf.subarray(8, 40)),
    createdBy: new PublicKey(buf.subarray(40, 72)),
    matchedLoanSequence: new BN(buf.subarray(72, 80), 'le'),
    borrowerMarginfiBorrowShares: readU128(buf, 80),
    debtIndexAtLastAccrual: readU128(buf, 96),
    principalDebtAtoms: new BN(buf.subarray(112, 120), 'le'),
    outstandingDebtAtoms: new BN(buf.subarray(120, 128), 'le'),
    lenderClaimableAtoms: new BN(buf.subarray(128, 136), 'le'),
    collateralAtoms: new BN(buf.subarray(136, 144), 'le'),
    accumulatedProtocolFeeAtoms: new BN(buf.subarray(144, 152), 'le'),
    accumulatedCuratorFeeAtoms: new BN(buf.subarray(152, 160), 'le'),
    startedAtUnix: readI64(buf, 160),
    maturesAtUnix: readI64(buf, 168),
    lastAccruedUnix: readI64(buf, 176),
    lenderSeatIndex: buf.readUInt32LE(184),
    borrowerSeatIndex: buf.readUInt32LE(188),
    borrowerRateBps: buf.readUInt16LE(192),
    lenderRateBps: buf.readUInt16LE(194),
    state: buf.readUInt8(196) as LoanState,
    loanType: buf.readUInt8(197) as LoanType,
    flags: buf.readUInt8(198),
    version: buf.readUInt8(199),
    bump: buf.readUInt8(200),
    lenderKind: buf.readUInt8(201) as LenderKind,
    lenderProfileId: buf.readUInt8(202),
    hasRestingSecondaryBid: buf.readUInt8(203) !== 0,
    curatorFeeBpsSnapshot: buf.readUInt16LE(204),
    lenderGlobalVault: new PublicKey(buf.subarray(208, 240)),
    principalRetiredAtoms: new BN(buf.subarray(240, 248), 'le'),
    lenderDebtSharePriceSnapshotFp48: readU128(buf, 256),
    borrowerCollateralSharePriceSnapshotFp48: readU128(buf, 272),
  };
}
