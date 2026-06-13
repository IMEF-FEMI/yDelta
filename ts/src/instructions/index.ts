/**
 * Re-exports every yDelta instruction builder + the supporting helpers.
 *
 * One file per Rust `instruction_builders/*.rs`, with the multi-builder files
 * (`adminTransfers`, `globalConfigAdmin`, `checkLiquidatable`) grouping the
 * tags that the Rust source bundles together.
 *
 * 45 instructions total — tags 0..=44, contiguous (see `_tags.ts`).
 */
export { InstructionTag } from './_tags.js';
export { cuBudgetIx, withCuBudget } from './_helpers.js';

export { createMarketInstruction } from './createMarket.js';
export type { CreateMarketArgs } from './createMarket.js';

export { claimSeatInstruction } from './claimSeat.js';

export { claimSeatIfNeededInstructions, hasUserSeat } from './claimSeatIfNeeded.js';
export type { ClaimSeatIfNeededArgs } from './claimSeatIfNeeded.js';

export { depositInstruction } from './deposit.js';
export type { DepositArgs } from './deposit.js';

export { withdrawInstruction } from './withdraw.js';
export type { WithdrawArgs } from './withdraw.js';

export { placeOrderInstruction } from './placeOrder.js';
export type { PlaceOrderArgs } from './placeOrder.js';

export {
  processMatchedLoanInstruction,
} from './processMatchedLoan.js';
export type {
  ProcessMatchedLoanArgs,
  VaultSettleAccounts,
} from './processMatchedLoan.js';

export { repayInstruction } from './repay.js';
export type { RepayArgs } from './repay.js';

export { syncMarketPositionInstruction } from './syncMarketPosition.js';

export { createVaultInstruction } from './createVault.js';
export type { CreateVaultArgs } from './createVault.js';

export { createPoolSubVaultInstruction } from './createPoolSubVault.js';
export type { CreatePoolSubVaultArgs } from './createPoolSubVault.js';

export { createPrivateSubVaultInstruction } from './createPrivateSubVault.js';
export type { CreatePrivateSubVaultArgs } from './createPrivateSubVault.js';

export { removeSubVaultInstruction } from './removeSubVault.js';
export type { RemoveSubVaultArgs } from './removeSubVault.js';

export { globalVaultDepositInstruction } from './globalVaultDeposit.js';
export type { GlobalVaultDepositArgs } from './globalVaultDeposit.js';

export { globalVaultWithdrawInstruction } from './globalVaultWithdraw.js';
export type { GlobalVaultWithdrawArgs } from './globalVaultWithdraw.js';

export { placeOrderForSubVaultInstruction } from './placeOrderForSubVault.js';
export type { PlaceOrderForSubVaultArgs } from './placeOrderForSubVault.js';

export { cancelOrderForSubVaultInstruction } from './cancelOrderForSubVault.js';

export { updateOrderForSubVaultInstruction } from './updateOrderForSubVault.js';
export type { UpdateOrderForSubVaultArgs } from './updateOrderForSubVault.js';

export { claimCuratorFeeInstruction } from './claimCuratorFee.js';
export type { ClaimCuratorFeeArgs } from './claimCuratorFee.js';

export { settleMaturedLoanInstruction } from './settleMaturedLoan.js';
export type { SettleMaturedLoanArgs } from './settleMaturedLoan.js';

export { liquidateLoanInstruction } from './liquidateLoan.js';
export type { LiquidateLoanArgs } from './liquidateLoan.js';

export { setFeeConfigInstruction } from './setFeeConfig.js';
export type { SetFeeConfigArgs } from './setFeeConfig.js';

export { protocolFeeClaimInstruction } from './protocolFeeClaim.js';
export type { ProtocolFeeClaimArgs } from './protocolFeeClaim.js';

export { claimRepaymentForSubVaultInstruction } from './claimRepaymentForSubVault.js';
export type { ClaimRepaymentForSubVaultArgs } from './claimRepaymentForSubVault.js';

export {
  transferMarketAdminInstruction,
  acceptMarketAdminInstruction,
  transferGlobalVaultAdminInstruction,
  acceptGlobalVaultAdminInstruction,
  transferCuratorInstruction,
  acceptCuratorInstruction,
} from './adminTransfers.js';

export { setMarketPauseInstruction } from './setMarketPause.js';

export { createGlobalConfigInstruction } from './createGlobalConfig.js';
export type { CreateGlobalConfigArgs } from './createGlobalConfig.js';

export {
  transferProtocolAdminInstruction,
  acceptProtocolAdminInstruction,
  setGlobalPauseInstruction,
} from './globalConfigAdmin.js';

export { updateSubVaultInstruction } from './updateSubVault.js';
export type { UpdateSubVaultArgs } from './updateSubVault.js';

export { convertP2poolToFixedInstruction } from './convertP2poolToFixed.js';
export type { ConvertP2PoolToFixedArgs } from './convertP2poolToFixed.js';

export {
  checkLtvLiquidatableInstruction,
  checkMaturityLiquidatableInstruction,
} from './checkLiquidatable.js';
export type {
  CheckLtvLiquidatableArgs,
  CheckMaturityLiquidatableArgs,
} from './checkLiquidatable.js';

export { setVaultPauseInstruction } from './setVaultPause.js';

export { sunsetSubVaultInstruction } from './sunsetSubVault.js';
export type { SunsetSubVaultArgs } from './sunsetSubVault.js';

export { resumeSubVaultInstruction } from './resumeSubVault.js';
export type { ResumeSubVaultArgs } from './resumeSubVault.js';

export { adminCancelSubVaultOrderInstruction } from './adminCancelSubVaultOrder.js';
export type { AdminCancelSubVaultOrderArgs } from './adminCancelSubVaultOrder.js';

export { cancelOrderInstruction } from './cancelOrder.js';
export type { CancelOrderArgs } from './cancelOrder.js';

export { matchCrankInstruction } from './matchCrank.js';
export type { MatchCrankArgs } from './matchCrank.js';

export { updateOrderInstruction } from './updateOrder.js';
export type { UpdateOrderArgs } from './updateOrder.js';
