/**
 * `MatchedLoan` — transient queue node emitted by the matching engine on
 * each cross. 144 bytes. Promoted into a `LoanFixed` PDA by the
 * `process_matched_loan` cranker.
 *
 * Layout:
 *   @0   8    u64   sequence
 *   @8   8    u8[8] _pad0
 *   @16  16   u128  borrower_marginfi_borrow_shares  (P2Pool only; 0 for Fixed)
 *   @32  8    u64   principal_atoms                  (gross matched)
 *   @40  8    u64   origination_atoms                (deducted from borrower credit)
 *   @48  8    u64   collateral_atoms
 *   @56  8    i64   matched_at_unix
 *   @64  4    u32   lender_seat_index
 *   @68  4    u32   borrower_seat_index
 *   @72  4    u32   term_seconds
 *   @76  2    u16   borrower_rate_bps
 *   @78  2    u16   lender_rate_bps
 *   @80  1    u8    loan_type   (0 = Fixed, 1 = P2Pool)
 *   @81  1    u8    flags       (bit 0: VAULT_LENDER, bit 3: VAULT_PRESETTLED)
 *   @82  6    u8[6] _pad_before_p5
 *   @88  24   u8[24] _reserved
 *   @112 16   u128  lender_debt_share_price_snapshot_fp48
 *   @128 16   u128  borrower_collateral_share_price_snapshot_fp48
 */
import { LoanType } from '../types.js';
import { readI64, readU128, readU16, readU32, readU64, readU8 } from './_read.js';

export const MATCHED_LOAN_SIZE = 144;

export interface MatchedLoan {
  sequence: bigint;
  borrowerMarginfiBorrowShares: bigint;
  principalAtoms: bigint;
  originationAtoms: bigint;
  collateralAtoms: bigint;
  matchedAtUnix: bigint;
  lenderSeatIndex: number;
  borrowerSeatIndex: number;
  termSeconds: number;
  borrowerRateBps: number;
  lenderRateBps: number;
  loanType: LoanType;
  flags: number;
  lenderDebtSharePriceSnapshotFp48: bigint;
  borrowerCollateralSharePriceSnapshotFp48: bigint;
}

export function decodeMatchedLoan(payload: DataView): MatchedLoan {
  return {
    sequence: readU64(payload, 0),
    borrowerMarginfiBorrowShares: readU128(payload, 16),
    principalAtoms: readU64(payload, 32),
    originationAtoms: readU64(payload, 40),
    collateralAtoms: readU64(payload, 48),
    matchedAtUnix: readI64(payload, 56),
    lenderSeatIndex: readU32(payload, 64),
    borrowerSeatIndex: readU32(payload, 68),
    termSeconds: readU32(payload, 72),
    borrowerRateBps: readU16(payload, 76),
    lenderRateBps: readU16(payload, 78),
    loanType: readU8(payload, 80) as LoanType,
    flags: readU8(payload, 81),
    lenderDebtSharePriceSnapshotFp48: readU128(payload, 112),
    borrowerCollateralSharePriceSnapshotFp48: readU128(payload, 128),
  };
}
