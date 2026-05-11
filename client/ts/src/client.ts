import {
  Commitment,
  Connection,
  PublicKey,
  TransactionInstruction,
} from '@solana/web3.js';

import { YDELTA_PROGRAM_ID } from './utils/programId';
import {
  globalConfigPda,
  loanPda,
  splitLoanPda,
  marketSignerPda,
  marketVaultPda,
  userAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  vaultIntegrationAccountPda,
  vaultStagingPda,
  lenderMarginfiAccountPda,
  borrowerMarginfiAccountPda,
} from './utils/pdas';
import {
  decodeGlobalConfig,
  GlobalConfig,
  decodeUserAccountFixed,
  UserAccountFixed,
  decodeMarketFixed,
  MarketFixed,
  decodeLoanFixed,
  LoanFixed,
  decodeGlobalVaultFixed,
  GlobalVaultFixed,
} from './ydelta/accounts';

// Raw instruction builders
import * as Ix from './ydelta/instructions';

export type YdeltaClientOpts = {
  connection: Connection;
  programId?: PublicKey;
};

/** Thin façade exposing raw `*Ix` builders and on-chain account loads. */
export class YdeltaClient {
  readonly connection: Connection;
  readonly programId: PublicKey;

  constructor(opts: YdeltaClientOpts) {
    this.connection = opts.connection;
    this.programId = opts.programId ?? YDELTA_PROGRAM_ID;
  }

  // ─── PDAs ───

  globalConfigPda(): PublicKey {
    return globalConfigPda(this.programId)[0];
  }
  marketSignerPda(market: PublicKey): PublicKey {
    return marketSignerPda(market, this.programId)[0];
  }
  lenderMarginfiAccountPda(market: PublicKey): PublicKey {
    return lenderMarginfiAccountPda(market, this.programId)[0];
  }
  borrowerMarginfiAccountPda(market: PublicKey): PublicKey {
    return borrowerMarginfiAccountPda(market, this.programId)[0];
  }
  marketVaultPda(market: PublicKey, mint: PublicKey): PublicKey {
    return marketVaultPda(market, mint, this.programId)[0];
  }
  loanPda(market: PublicKey, sequence: bigint | number): PublicKey {
    return loanPda(market, sequence, this.programId)[0];
  }
  splitLoanPda(market: PublicKey, queueNodeSequence: bigint | number): PublicKey {
    return splitLoanPda(market, queueNodeSequence, this.programId)[0];
  }
  userAccountPda(owner: PublicKey): PublicKey {
    return userAccountPda(owner, this.programId)[0];
  }
  globalVaultPda(mint: PublicKey): PublicKey {
    return globalVaultPda(mint, this.programId)[0];
  }
  globalVaultSignerPda(vault: PublicKey): PublicKey {
    return globalVaultSignerPda(vault, this.programId)[0];
  }
  vaultIntegrationAccountPda(vault: PublicKey): PublicKey {
    return vaultIntegrationAccountPda(vault, this.programId)[0];
  }
  vaultStagingPda(vault: PublicKey): PublicKey {
    return vaultStagingPda(vault, this.programId)[0];
  }

  // ─── Account loads ───

  async loadGlobalConfig(commitment: Commitment = 'confirmed'): Promise<GlobalConfig | null> {
    const info = await this.connection.getAccountInfo(this.globalConfigPda(), commitment);
    return info ? decodeGlobalConfig(Buffer.from(info.data)) : null;
  }

  async loadMarket(market: PublicKey, commitment: Commitment = 'confirmed'): Promise<MarketFixed | null> {
    const info = await this.connection.getAccountInfo(market, commitment);
    return info ? decodeMarketFixed(Buffer.from(info.data)) : null;
  }

  async loadUserAccount(owner: PublicKey, commitment: Commitment = 'confirmed'): Promise<UserAccountFixed | null> {
    const info = await this.connection.getAccountInfo(this.userAccountPda(owner), commitment);
    return info ? decodeUserAccountFixed(Buffer.from(info.data)) : null;
  }

  async loadLoan(market: PublicKey, sequence: bigint | number, commitment: Commitment = 'confirmed'): Promise<LoanFixed | null> {
    const info = await this.connection.getAccountInfo(this.loanPda(market, sequence), commitment);
    return info ? decodeLoanFixed(Buffer.from(info.data)) : null;
  }

  async loadGlobalVault(mint: PublicKey, commitment: Commitment = 'confirmed'): Promise<GlobalVaultFixed | null> {
    const info = await this.connection.getAccountInfo(this.globalVaultPda(mint), commitment);
    return info ? decodeGlobalVaultFixed(Buffer.from(info.data)) : null;
  }

  // ─── Raw instruction builders (every tag 0–41) ───

  createMarketIx = (accounts: Ix.CreateMarketInstructionAccounts): TransactionInstruction =>
    Ix.createCreateMarketInstruction(accounts, this.programId);

  claimSeatIx = (accounts: Ix.ClaimSeatInstructionAccounts): TransactionInstruction =>
    Ix.createClaimSeatInstruction(accounts, this.programId);

  depositIx = (
    accounts: Ix.DepositInstructionAccounts,
    args: Ix.DepositInstructionArgs,
  ): TransactionInstruction => Ix.createDepositInstruction(accounts, args, this.programId);

  withdrawIx = (
    accounts: Ix.WithdrawInstructionAccounts,
    args: Ix.WithdrawInstructionArgs,
  ): TransactionInstruction => Ix.createWithdrawInstruction(accounts, args, this.programId);

  placeOrderIx = (
    accounts: Ix.PlaceOrderInstructionAccounts,
    args: Ix.PlaceOrderInstructionArgs,
  ): TransactionInstruction => Ix.createPlaceOrderInstruction(accounts, args, this.programId);

  cancelOrderIx = (
    accounts: Ix.CancelOrderInstructionAccounts,
    args: Ix.CancelOrderInstructionArgs,
  ): TransactionInstruction => Ix.createCancelOrderInstruction(accounts, args, this.programId);

  updateOrderIx = (
    accounts: Ix.UpdateOrderInstructionAccounts,
    args: Ix.UpdateOrderInstructionArgs,
  ): TransactionInstruction => Ix.createUpdateOrderInstruction(accounts, args, this.programId);

  processMatchedLoanIx = (
    accounts: Ix.ProcessMatchedLoanInstructionAccounts,
    args: Ix.ProcessMatchedLoanInstructionArgs,
  ): TransactionInstruction =>
    Ix.createProcessMatchedLoanInstruction(accounts, args, this.programId);

  repayIx = (
    accounts: Ix.RepayInstructionAccounts,
    args: Ix.RepayInstructionArgs,
  ): TransactionInstruction => Ix.createRepayInstruction(accounts, args, this.programId);

  claimRepaymentIx = (
    accounts: Ix.ClaimRepaymentInstructionAccounts,
    args: Ix.ClaimRepaymentInstructionArgs,
  ): TransactionInstruction => Ix.createClaimRepaymentInstruction(accounts, args, this.programId);

  syncMarketPositionIx = (accounts: Ix.SyncMarketPositionInstructionAccounts): TransactionInstruction =>
    Ix.createSyncMarketPositionInstruction(accounts, this.programId);

  createVaultIx = (accounts: Ix.CreateVaultInstructionAccounts): TransactionInstruction =>
    Ix.createCreateVaultInstruction(accounts, this.programId);

  createRiskProfileIx = (
    accounts: Ix.CreateRiskProfileInstructionAccounts,
    args: Ix.CreateRiskProfileInstructionArgs,
  ): TransactionInstruction =>
    Ix.createCreateRiskProfileInstruction(accounts, args, this.programId);

  globalVaultDepositIx = (
    accounts: Ix.GlobalVaultDepositInstructionAccounts,
    args: Ix.GlobalVaultDepositInstructionArgs,
  ): TransactionInstruction =>
    Ix.createGlobalVaultDepositInstruction(accounts, args, this.programId);

  globalVaultWithdrawIx = (
    accounts: Ix.GlobalVaultWithdrawInstructionAccounts,
    args: Ix.GlobalVaultWithdrawInstructionArgs,
  ): TransactionInstruction =>
    Ix.createGlobalVaultWithdrawInstruction(accounts, args, this.programId);

  claimSeatForRiskProfileIx = (
    accounts: Ix.ClaimSeatForRiskProfileInstructionAccounts,
    args: Ix.ClaimSeatForRiskProfileInstructionArgs,
  ): TransactionInstruction =>
    Ix.createClaimSeatForRiskProfileInstruction(accounts, args, this.programId);

  placeOrderForRiskProfileIx = (
    accounts: Ix.PlaceOrderForRiskProfileInstructionAccounts,
    args: Ix.PlaceOrderForRiskProfileInstructionArgs,
  ): TransactionInstruction =>
    Ix.createPlaceOrderForRiskProfileInstruction(accounts, args, this.programId);

  cancelOrderForRiskProfileIx = (
    accounts: Ix.CancelOrderForRiskProfileInstructionAccounts,
    args: Ix.CancelOrderForRiskProfileInstructionArgs,
  ): TransactionInstruction =>
    Ix.createCancelOrderForRiskProfileInstruction(accounts, args, this.programId);

  updateOrderForRiskProfileIx = (
    accounts: Ix.UpdateOrderForRiskProfileInstructionAccounts,
    args: Ix.UpdateOrderForRiskProfileInstructionArgs,
  ): TransactionInstruction =>
    Ix.createUpdateOrderForRiskProfileInstruction(accounts, args, this.programId);

  claimCuratorFeeIx = (
    accounts: Ix.ClaimCuratorFeeInstructionAccounts,
    args: Ix.ClaimCuratorFeeInstructionArgs,
  ): TransactionInstruction =>
    Ix.createClaimCuratorFeeInstruction(accounts, args, this.programId);

  settleMaturedLoanIx = (
    accounts: Ix.SettleMaturedLoanInstructionAccounts,
    args: Ix.SettleMaturedLoanInstructionArgs,
  ): TransactionInstruction =>
    Ix.createSettleMaturedLoanInstruction(accounts, args, this.programId);

  liquidateLoanIx = (
    accounts: Ix.LiquidateLoanInstructionAccounts,
    args: Ix.LiquidateLoanInstructionArgs,
  ): TransactionInstruction =>
    Ix.createLiquidateLoanInstruction(accounts, args, this.programId);

  setFeeConfigIx = (
    accounts: Ix.SetFeeConfigInstructionAccounts,
    args: Ix.SetFeeConfigInstructionArgs,
  ): TransactionInstruction => Ix.createSetFeeConfigInstruction(accounts, args, this.programId);

  protocolFeeClaimIx = (accounts: Ix.ProtocolFeeClaimInstructionAccounts): TransactionInstruction =>
    Ix.createProtocolFeeClaimInstruction(accounts, this.programId);

  claimRepaymentForRiskProfileIx = (
    accounts: Ix.ClaimRepaymentForRiskProfileInstructionAccounts,
  ): TransactionInstruction =>
    Ix.createClaimRepaymentForRiskProfileInstruction(accounts, this.programId);

  transferMarketAdminIx = (
    accounts: Ix.TransferMarketAdminInstructionAccounts,
    args: { newAdmin: PublicKey },
  ): TransactionInstruction =>
    Ix.createTransferMarketAdminInstruction(accounts, args, this.programId);

  acceptMarketAdminIx = (
    accounts: Ix.AcceptMarketAdminInstructionAccounts,
  ): TransactionInstruction =>
    Ix.createAcceptMarketAdminInstruction(accounts, this.programId);

  transferGlobalVaultAdminIx = (
    accounts: Ix.TransferGlobalVaultAdminInstructionAccounts,
    args: { newAdmin: PublicKey },
  ): TransactionInstruction =>
    Ix.createTransferGlobalVaultAdminInstruction(accounts, args, this.programId);

  acceptGlobalVaultAdminIx = (
    accounts: Ix.AcceptGlobalVaultAdminInstructionAccounts,
  ): TransactionInstruction =>
    Ix.createAcceptGlobalVaultAdminInstruction(accounts, this.programId);

  transferCuratorIx = (
    accounts: Ix.TransferCuratorInstructionAccounts,
    args: { profileId: number; newCurator: PublicKey },
  ): TransactionInstruction =>
    Ix.createTransferCuratorInstruction(accounts, args, this.programId);

  acceptCuratorIx = (
    accounts: Ix.AcceptCuratorInstructionAccounts,
    args: { profileId: number },
  ): TransactionInstruction => Ix.createAcceptCuratorInstruction(accounts, args, this.programId);

  setMarketPauseIx = (
    accounts: Ix.SetMarketPauseInstructionAccounts,
    args: { paused: boolean },
  ): TransactionInstruction => Ix.createSetMarketPauseInstruction(accounts, args, this.programId);

  createGlobalConfigIx = (accounts: Ix.CreateGlobalConfigInstructionAccounts): TransactionInstruction =>
    Ix.createCreateGlobalConfigInstruction(accounts, this.programId);

  transferProtocolAdminIx = (
    accounts: Ix.TransferProtocolAdminInstructionAccounts,
    args: { newAdmin: PublicKey },
  ): TransactionInstruction =>
    Ix.createTransferProtocolAdminInstruction(accounts, args, this.programId);

  acceptProtocolAdminIx = (
    accounts: Ix.AcceptProtocolAdminInstructionAccounts,
  ): TransactionInstruction =>
    Ix.createAcceptProtocolAdminInstruction(accounts, this.programId);

  setGlobalPauseIx = (
    accounts: Ix.SetGlobalPauseInstructionAccounts,
    args: { paused: boolean },
  ): TransactionInstruction => Ix.createSetGlobalPauseInstruction(accounts, args, this.programId);

  updateRiskProfileIx = (
    accounts: Ix.UpdateRiskProfileInstructionAccounts,
    args: Parameters<typeof Ix.createUpdateRiskProfileInstruction>[1],
  ): TransactionInstruction =>
    Ix.createUpdateRiskProfileInstruction(accounts, args, this.programId);

  releaseSeatForRiskProfileIx = (
    accounts: Ix.ReleaseSeatForRiskProfileInstructionAccounts,
    args: { profileId: number },
  ): TransactionInstruction =>
    Ix.createReleaseSeatForRiskProfileInstruction(accounts, args, this.programId);

  syncMarketSeatsForRiskProfileIx = (
    accounts: Ix.SyncMarketSeatsForRiskProfileInstructionAccounts,
    args: { profileId: number },
  ): TransactionInstruction =>
    Ix.createSyncMarketSeatsForRiskProfileInstruction(accounts, args, this.programId);

  convertP2PoolToFixedIx = (
    accounts: Ix.ConvertP2PoolToFixedInstructionAccounts,
    args: { maxAcceptableRateBps: number },
  ): TransactionInstruction =>
    Ix.createConvertP2PoolToFixedInstruction(accounts, args, this.programId);

  checkLtvLiquidatableIx = (
    accounts: Ix.CheckLtvLiquidatableInstructionAccounts,
  ): TransactionInstruction =>
    Ix.createCheckLtvLiquidatableInstruction(accounts, this.programId);

  checkMaturityLiquidatableIx = (
    accounts: Ix.CheckMaturityLiquidatableInstructionAccounts,
  ): TransactionInstruction =>
    Ix.createCheckMaturityLiquidatableInstruction(accounts, this.programId);
}
