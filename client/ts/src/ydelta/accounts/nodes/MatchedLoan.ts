import BN from 'bn.js';

export const MATCHED_LOAN_SIZE = 144;

export const MATCHED_LOAN_FLAG_VAULT_LENDER = 0b0000_0001;
export const MATCHED_LOAN_FLAG_SECONDARY = 0b0000_0010;
export const MATCHED_LOAN_FLAG_SECONDARY_SPLIT = 0b0000_0100;
export const MATCHED_LOAN_FLAG_VAULT_MAKER_FULLY_FILLED = 0b0000_1000;

export type MatchedLoan = {
  /** Tree key. */
  sequence: BN;
  /** fp48 marginfi share quantity (raw I80F48 mantissa as u128) */
  borrowerMarginfiBorrowShares: BN;
  /** atoms */
  principalAtoms: BN;
  /** atoms */
  originationAtoms: BN;
  /** atoms */
  collateralAtoms: BN;
  /** unix seconds */
  matchedAtUnix: BN;
  lenderSeatIndex: number;
  borrowerSeatIndex: number;
  /** seconds */
  termSeconds: number;
  /** bps */
  borrowerRateBps: number;
  /** bps */
  lenderRateBps: number;
  /** 0 = Fixed, 1 = P2Pool. */
  loanType: number;
  /** bit field: see MATCHED_LOAN_FLAG_* consts */
  flags: number;
  /** Populated only when SECONDARY flag is set. */
  referencedLoanSequence: BN;
  /** atoms — secondary-only (zero on primary matches) */
  cashPaidAtoms: BN;
  newLenderSeatIndex: number;
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
  const TWO_64 = new BN(1).shln(64);
  return u.testn(63) ? u.sub(TWO_64) : u;
}

export function decodeMatchedLoan(buf: Buffer): MatchedLoan {
  return {
    sequence: new BN(buf.subarray(0, 8), 'le'),
    borrowerMarginfiBorrowShares: readU128(buf, 16),
    principalAtoms: new BN(buf.subarray(32, 40), 'le'),
    originationAtoms: new BN(buf.subarray(40, 48), 'le'),
    collateralAtoms: new BN(buf.subarray(48, 56), 'le'),
    matchedAtUnix: readI64(buf, 56),
    lenderSeatIndex: buf.readUInt32LE(64),
    borrowerSeatIndex: buf.readUInt32LE(68),
    termSeconds: buf.readUInt32LE(72),
    borrowerRateBps: buf.readUInt16LE(76),
    lenderRateBps: buf.readUInt16LE(78),
    loanType: buf.readUInt8(80),
    flags: buf.readUInt8(81),
    referencedLoanSequence: new BN(buf.subarray(88, 96), 'le'),
    cashPaidAtoms: new BN(buf.subarray(96, 104), 'le'),
    newLenderSeatIndex: buf.readUInt32LE(104),
    lenderDebtSharePriceSnapshotFp48: readU128(buf, 112),
    borrowerCollateralSharePriceSnapshotFp48: readU128(buf, 128),
  };
}

export function matchedLoanIsSecondary(node: MatchedLoan): boolean {
  return (node.flags & MATCHED_LOAN_FLAG_SECONDARY) !== 0;
}

export function matchedLoanIsSecondarySplit(node: MatchedLoan): boolean {
  return (node.flags & MATCHED_LOAN_FLAG_SECONDARY_SPLIT) !== 0;
}

export function matchedLoanIsVaultLender(node: MatchedLoan): boolean {
  return (node.flags & MATCHED_LOAN_FLAG_VAULT_LENDER) !== 0;
}

export function matchedLoanVaultMakerFullyFilled(node: MatchedLoan): boolean {
  return (node.flags & MATCHED_LOAN_FLAG_VAULT_MAKER_FULLY_FILLED) !== 0;
}
