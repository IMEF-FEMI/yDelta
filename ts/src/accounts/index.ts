/** Phase-3 zero-copy account decoders. */
export * from './_read.js';
export * from './layout.js';
export * from './trees.js';

export { decodeGlobalConfig, GLOBAL_CONFIG_SIZE } from './globalConfig.js';
export type { GlobalConfig } from './globalConfig.js';

export { decodeClaimedSeat, CLAIMED_SEAT_SIZE } from './claimedSeat.js';
export type { ClaimedSeat } from './claimedSeat.js';

export { decodeRestingOrder, RESTING_ORDER_SIZE } from './restingOrder.js';
export type { RestingOrder } from './restingOrder.js';

export { decodeMatchedLoan, MATCHED_LOAN_SIZE } from './matchedLoan.js';
export type { MatchedLoan } from './matchedLoan.js';

export {
  decodeSubVault,
  decodeSubVaultDepositorSeat,
  decodeSubVaultOrderRef,
  SUB_VAULT_SIZE,
  SUB_VAULT_DEPOSITOR_SEAT_SIZE,
  SUB_VAULT_ORDER_REF_SIZE,
} from './subVault.js';
export type {
  SubVault,
  SubVaultDepositorSeat,
  SubVaultOrderRef,
} from './subVault.js';

export {
  decodeLoanFixed,
  projectLenderClaimable,
  projectOutstandingDebt,
  LOAN_FIXED_SIZE,
  LOAN_FIXED_DISCRIMINANT,
  LOAN_SECONDS_PER_YEAR,
  LOAN_BPS_PER_UNIT,
} from './loan.js';
export type { LoanFixed } from './loan.js';

export {
  decodeUserAccount,
  decodeUserAccountHeader,
  decodeVaultPosition,
  decodeMarketPosition,
  decodeUserLoanRef,
  decodeUserOrderRef,
  USER_ACCOUNT_FIXED_SIZE,
  USER_ACCOUNT_BLOCK_PAYLOAD_SIZE,
  USER_ORDER_REF_SIZE,
} from './userAccount.js';
export type {
  UserAccount,
  UserAccountHeader,
  VaultPosition,
  MarketPosition,
  UserLoanRef,
  UserOrderRef,
} from './userAccount.js';

export {
  decodeMarket,
  decodeMarketHeader,
  iterAsks,
  iterBids,
  iterClaimedSeats,
  iterMatchedLoans,
  iterFreeList,
  lookupClaimedSeatAt,
  dynamicRegion as marketDynamicRegion,
  MARKET_FIXED_SIZE,
  MARKET_BLOCK_PAYLOAD_SIZE,
  MARKET_FIXED_DISCRIMINANT,
} from './market.js';
export type { Market, MarketHeader, FeeConfig } from './market.js';

export {
  decodeGlobalVault,
  decodeGlobalVaultHeader,
  iterSubVaults,
  iterDepositorSeats,
  iterMarketOrders,
  iterSubVaultFreeList,
  iterNodeFreeList,
  vaultDynamicRegion,
  GLOBAL_VAULT_FIXED_SIZE,
  GLOBAL_VAULT_DISCRIMINANT,
} from './vault.js';
export type { GlobalVault, GlobalVaultHeader } from './vault.js';
