export enum Side {
  Bid = 0,
  Ask = 1,
}

export enum OrderType {
  Limit = 0,
  ImmediateOrCancel = 1,
  PostOnly = 2,
}

export enum OrderKind {
  Primary = 0,
  SecondaryLoanSale = 1,
}

export enum OwnerKind {
  User = 0,
  Vault = 1,
}

export enum LoanState {
  Active = 0,
  /** discriminant 3 — reserved 1 and 2 for future intermediate states. */
  Repaid = 3,
}

export enum LoanType {
  Fixed = 0,
  P2Pool = 1,
}

export enum LenderKind {
  User = 0,
  Vault = 1,
}

export enum CounterpartyKind {
  User = 0,
  Vault = 1,
}

export enum UserLoanRole {
  Borrower = 0,
  Lender = 1,
}

export function orderTypeCanRest(orderType: OrderType): boolean {
  return orderType !== OrderType.ImmediateOrCancel;
}

export function orderTypeCanTake(orderType: OrderType): boolean {
  return orderType !== OrderType.PostOnly;
}
