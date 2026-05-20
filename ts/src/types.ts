/** Side of a resting order on the book. */
export enum Side {
  Bid = 0,
  Ask = 1,
}

/** Order time-in-force for `place_order`. */
export enum OrderType {
  Limit = 0,
  ImmediateOrCancel = 1,
  PostOnly = 2,
}

/** Loan funding source. */
export enum LoanType {
  Fixed = 0,
  P2Pool = 1,
}

/** Lifecycle state of a `LoanFixed`. */
export enum LoanState {
  Active = 0,
  Repaid = 3,
}

/** Owner-kind discriminant stored on `ClaimedSeat`. */
export enum OwnerKind {
  User = 0,
  RiskProfile = 1,
}

/** Place-order `flags` bits. */
export const FLAG_OB_ONLY = 0b0000_0010;

/** `MatchedLoan.flags` bits. */
export const MATCHED_LOAN_FLAG_VAULT_LENDER = 0b0000_0001;
export const MATCHED_LOAN_FLAG_VAULT_PRESETTLED = 0b0000_1000;
