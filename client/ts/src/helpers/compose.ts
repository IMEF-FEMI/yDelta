import {
  Connection,
  PublicKey,
  TransactionInstruction,
} from '@solana/web3.js';
import BN from 'bn.js';
import { YDELTA_PROGRAM_ID } from '../utils/programId';
import { Market } from '../market';
import { Loan } from '../loan';
import {
  createClaimSeatInstruction,
  createDepositInstruction,
  createWithdrawInstruction,
  createPlaceOrderInstruction,
  createCancelOrderInstruction,
  createUpdateOrderInstruction,
  createRepayInstruction,
  createClaimRepaymentInstruction,
  createSettleMaturedLoanInstruction,
  createLiquidateLoanInstruction,
  createProtocolFeeClaimInstruction,
  createConvertP2PoolToFixedInstruction,
  createSetMarketPauseInstruction,
  createSetGlobalPauseInstruction,
  createGlobalVaultDepositInstruction,
  createGlobalVaultWithdrawInstruction,
  createPlaceOrderForRiskProfileInstruction,
  createCancelOrderForRiskProfileInstruction,
  createUpdateOrderForRiskProfileInstruction,
  createClaimSeatForRiskProfileInstruction,
  createReleaseSeatForRiskProfileInstruction,
  createSyncMarketSeatsForRiskProfileInstruction,
  createCreateRiskProfileInstruction,
  createUpdateRiskProfileInstruction,
  createClaimCuratorFeeInstruction,
  createClaimRepaymentForRiskProfileInstruction,
  createTransferMarketAdminInstruction,
  createAcceptMarketAdminInstruction,
  createTransferGlobalVaultAdminInstruction,
  createAcceptGlobalVaultAdminInstruction,
  createTransferCuratorInstruction,
  createAcceptCuratorInstruction,
  createTransferProtocolAdminInstruction,
  createAcceptProtocolAdminInstruction,
  PlaceOrderInstructionArgs,
  CancelOrderInstructionArgs,
  UpdateOrderInstructionArgs,
} from '../ydelta/instructions';
import { Side, OrderKind, OrderType } from '../ydelta/types/enums';
import { createAtaIfMissing, TOKEN_PROGRAM_ID } from '../utils/solana';
import { globalVaultPda } from '../utils/pdas';

export type ClaimSeatComposeArgs = {
  owner: PublicKey;
  market: Market;
  programId?: PublicKey;
};

/** Returns a `ClaimSeat` ix only if the market has no seat for `owner`. The
 *  seat check is buffer-side (no RPC); pass a freshly-loaded `Market`. */
export function claimSeatIfMissing({
  owner,
  market,
  programId = YDELTA_PROGRAM_ID,
}: ClaimSeatComposeArgs): TransactionInstruction[] {
  const existing = market.seatFor(owner, 0);
  if (existing) return [];
  return [createClaimSeatInstruction({ payer: owner, market: market.address }, programId)];
}

export type DepositComposeArgs = {
  connection: Connection;
  payer: PublicKey;
  market: Market;
  /** Deposit `debt` to fund the lending side, `collateral` to back a borrow. */
  side: 'debt' | 'collateral';
  amountAtoms: BN | bigint | number;
  marginfiGroup: PublicKey;
  bank: PublicKey;
  liquidityVault: PublicKey;
  marginfiProgram: PublicKey;
  tokenProgram?: PublicKey;
  /** Optional pre-resolved trader ATA. When omitted, the SDK derives it and
   *  prepends a `createATA` ix when missing. */
  payerToken?: PublicKey;
  programId?: PublicKey;
};

/** Compose `[createATA?, ClaimSeat?, Deposit]` so callers don't have to
 *  pre-flight the seat/ATA state themselves. */
export async function depositWithATAIxs(args: DepositComposeArgs): Promise<TransactionInstruction[]> {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  const tokenProgram = args.tokenProgram ?? TOKEN_PROGRAM_ID;
  const mint = args.side === 'debt' ? args.market.debtMint() : args.market.collateralMint();

  const ixs: TransactionInstruction[] = [];

  let traderToken = args.payerToken;
  if (!traderToken) {
    const { ata, createIx } = await createAtaIfMissing(
      args.connection,
      args.payer,
      args.payer,
      mint,
      tokenProgram,
    );
    traderToken = ata;
    if (createIx) ixs.push(createIx);
  }

  ixs.push(
    ...claimSeatIfMissing({
      owner: args.payer,
      market: args.market,
      programId,
    }),
  );

  const amount = BN.isBN(args.amountAtoms)
    ? args.amountAtoms
    : new BN(args.amountAtoms.toString());

  ixs.push(
    createDepositInstruction(
      {
        payer: args.payer,
        market: args.market.address,
        mint,
        debtMint: args.market.debtMint(),
        traderToken,
        tokenProgram,
        marginfiGroup: args.marginfiGroup,
        bank: args.bank,
        liquidityVault: args.liquidityVault,
        marginfiProgram: args.marginfiProgram,
      },
      { amountAtoms: amount, traderIndexHint: null },
      programId,
    ),
  );

  return ixs;
}

export type PlaceOrderComposeArgs = {
  market: Market;
  payer: PublicKey;
  marginfiGroup: PublicKey;
  debtBank: PublicKey;
  collateralBank: PublicKey;
  debtOracles: PublicKey[];
  collateralOracles: PublicKey[];
  debtLiquidityVault: PublicKey;
  debtBankLiquidityVaultAuthority: PublicKey;
  borrowerDebtToken: PublicKey;
  tokenProgram?: PublicKey;
  marginfiProgram: PublicKey;
  programId?: PublicKey;
  /** Loan PDA when placing a `SecondaryLoanSale` bid. */
  secondaryLoan?: PublicKey;
};

export type PlaceOrderFields = {
  rateBps: number;
  termSeconds: number;
  principalAtoms: BN | bigint | number;
  /** Required for Bids; must be 0 for Asks. */
  collateralAtoms: BN | bigint | number;
  /** Default Limit. */
  orderType?: OrderType;
  /** Borsh-Option<u16> on the wire. `null` defaults to marginfi-init LTV. */
  borrowerLtvBps?: number | null;
  /** 0 = no expiry. */
  lastValidUnixTs?: BN | bigint | number;
  /** Raw flags byte. */
  flags?: number;
  /** Pass when reusing a known seat index to skip the seat-tree walk. */
  seatIndexHint?: number | null;
};

function toBN(v: BN | bigint | number): BN {
  return BN.isBN(v) ? v : new BN(v.toString());
}

function placeOrderArgs(side: Side, kind: OrderKind, f: PlaceOrderFields): PlaceOrderInstructionArgs {
  return {
    seatIndexHint: f.seatIndexHint ?? null,
    side,
    orderType: f.orderType ?? OrderType.Limit,
    flags: f.flags ?? 0,
    kind,
    rateBps: f.rateBps,
    termSeconds: f.termSeconds,
    principalAtoms: toBN(f.principalAtoms),
    collateralAtoms: toBN(f.collateralAtoms),
    askingPriceAtoms: new BN(0),
    lastValidUnixTs: toBN(f.lastValidUnixTs ?? 0),
    borrowerLtvBps: f.borrowerLtvBps ?? null,
  };
}

function placeOrderIxsImpl(
  compose: PlaceOrderComposeArgs,
  side: Side,
  fields: PlaceOrderFields,
): TransactionInstruction[] {
  const programId = compose.programId ?? YDELTA_PROGRAM_ID;
  const ixs: TransactionInstruction[] = claimSeatIfMissing({
    owner: compose.payer,
    market: compose.market,
    programId,
  });
  const kind = compose.secondaryLoan ? OrderKind.SecondaryLoanSale : OrderKind.Primary;
  ixs.push(
    createPlaceOrderInstruction(
      {
        payer: compose.payer,
        market: compose.market.address,
        marginfiGroup: compose.marginfiGroup,
        debtBank: compose.debtBank,
        collateralBank: compose.collateralBank,
        debtOracles: compose.debtOracles,
        collateralOracles: compose.collateralOracles,
        debtLiquidityVault: compose.debtLiquidityVault,
        debtBankLiquidityVaultAuthority: compose.debtBankLiquidityVaultAuthority,
        borrowerDebtToken: compose.borrowerDebtToken,
        debtMint: compose.market.debtMint(),
        tokenProgram: compose.tokenProgram ?? TOKEN_PROGRAM_ID,
        marginfiProgram: compose.marginfiProgram,
        secondaryLoan: compose.secondaryLoan,
      },
      placeOrderArgs(side, kind, fields),
      programId,
    ),
  );
  return ixs;
}

/** Place a primary bid. Auto-prepends `ClaimSeat` when the trader has no seat
 *  in `market`. */
export function placeBidIxs(
  compose: PlaceOrderComposeArgs,
  fields: PlaceOrderFields,
): TransactionInstruction[] {
  return placeOrderIxsImpl(compose, Side.Bid, fields);
}

/** Place a primary ask. Asks must not carry collateral (`collateralAtoms = 0`). */
export function placeAskIxs(
  compose: PlaceOrderComposeArgs,
  fields: Omit<PlaceOrderFields, 'collateralAtoms'>,
): TransactionInstruction[] {
  return placeOrderIxsImpl(compose, Side.Ask, { ...fields, collateralAtoms: 0 });
}

export type CancelOrderComposeArgs = {
  market: Market;
  payer: PublicKey;
  orderSequenceNumber: BN | bigint | number;
  /** Pass the resting order's index in the dynamic region when known. Otherwise
   *  the program will fall back to a tree walk. */
  orderIndexHint?: number | null;
  seatIndexHint?: number | null;
  /** For SecondaryLoanSale cancels, the loan PDA the bid references. */
  secondaryLoan?: PublicKey;
  programId?: PublicKey;
};

export function cancelOrderIxs(args: CancelOrderComposeArgs): TransactionInstruction[] {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  const params: CancelOrderInstructionArgs = {
    orderSequenceNumber: toBN(args.orderSequenceNumber),
    orderIndexHint: args.orderIndexHint ?? null,
    seatIndexHint: args.seatIndexHint ?? null,
  };
  return [
    createCancelOrderInstruction(
      {
        payer: args.payer,
        market: args.market.address,
        secondaryLoan: args.secondaryLoan,
      },
      params,
      programId,
    ),
  ];
}

export type UpdateOrderComposeArgs = {
  market: Market;
  payer: PublicKey;
  programId?: PublicKey;
  args: UpdateOrderInstructionArgs;
};

export function updateOrderIxs(input: UpdateOrderComposeArgs): TransactionInstruction[] {
  const programId = input.programId ?? YDELTA_PROGRAM_ID;
  return [
    createUpdateOrderInstruction(
      { payer: input.payer, market: input.market.address },
      input.args,
      programId,
    ),
  ];
}

export type WithdrawComposeArgs = {
  connection: Connection;
  payer: PublicKey;
  market: Market;
  side: 'debt' | 'collateral';
  amountAtoms: BN | bigint | number;
  marginfiGroup: PublicKey;
  debtBank: PublicKey;
  collateralBank: PublicKey;
  liquidityVault: PublicKey;
  bankLiquidityVaultAuthority: PublicKey;
  debtOracles: PublicKey[];
  collateralOracles: PublicKey[];
  marginfiProgram: PublicKey;
  tokenProgram?: PublicKey;
  payerToken?: PublicKey;
  programId?: PublicKey;
};

/** Compose `[createATA?, Withdraw]`. */
export async function withdrawWithATAIxs(args: WithdrawComposeArgs): Promise<TransactionInstruction[]> {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  const tokenProgram = args.tokenProgram ?? TOKEN_PROGRAM_ID;
  const mint = args.side === 'debt' ? args.market.debtMint() : args.market.collateralMint();
  const ixs: TransactionInstruction[] = [];
  let traderToken = args.payerToken;
  if (!traderToken) {
    const { ata, createIx } = await createAtaIfMissing(args.connection, args.payer, args.payer, mint, tokenProgram);
    traderToken = ata;
    if (createIx) ixs.push(createIx);
  }
  ixs.push(
    createWithdrawInstruction(
      {
        payer: args.payer,
        market: args.market.address,
        mint,
        debtMint: args.market.debtMint(),
        traderToken,
        tokenProgram,
        marginfiGroup: args.marginfiGroup,
        debtBank: args.debtBank,
        collateralBank: args.collateralBank,
        liquidityVault: args.liquidityVault,
        bankLiquidityVaultAuthority: args.bankLiquidityVaultAuthority,
        debtOracles: args.debtOracles,
        collateralOracles: args.collateralOracles,
        marginfiProgram: args.marginfiProgram,
      },
      { amountAtoms: toBN(args.amountAtoms), traderIndexHint: null },
      programId,
    ),
  );
  return ixs;
}

// ────────────────────── Repay / Claim / Settlement ──────────────────────

export type RepayComposeArgs = {
  market: Market;
  loan: Loan;
  borrower: PublicKey;
  /** Atoms to repay. Ignored when `fullRepay` is true. */
  repayAtoms: BN | bigint | number;
  fullRepay: boolean;
  borrowerToken: PublicKey;
  marginfiGroup: PublicKey;
  debtBank: PublicKey;
  debtLiquidityVault: PublicKey;
  collateralBank: PublicKey;
  marginfiProgram: PublicKey;
  tokenProgram?: PublicKey;
  borrowerSeatIndexHint?: number | null;
  programId?: PublicKey;
};

/** Compose `Repay`. Atoms route to `market.lender_integration_account`
 *  regardless of `loan.lenderKind`; vault-funded loans drain to the
 *  GlobalVault via a subsequent `ClaimRepaymentForRiskProfile`. */
export function repayIxs(args: RepayComposeArgs): TransactionInstruction[] {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return [
    createRepayInstruction(
      {
        borrower: args.borrower,
        market: args.market.address,
        sequence: args.loan.data().matchedLoanSequence,
        debtMint: args.market.debtMint(),
        borrowerToken: args.borrowerToken,
        tokenProgram: args.tokenProgram ?? TOKEN_PROGRAM_ID,
        marginfiGroup: args.marginfiGroup,
        debtBank: args.debtBank,
        debtLiquidityVault: args.debtLiquidityVault,
        collateralBank: args.collateralBank,
        marginfiProgram: args.marginfiProgram,
      },
      {
        repayAtoms: toBN(args.repayAtoms),
        fullRepay: args.fullRepay,
        borrowerSeatIndexHint: args.borrowerSeatIndexHint ?? null,
      },
      programId,
    ),
  ];
}

export type ClaimRepaymentComposeArgs = {
  market: Market;
  loan: Loan;
  lender: PublicKey;
  debtBank: PublicKey;
  marginfiProgram: PublicKey;
  /** Pass `loan.data().createdBy` to refund the registering cranker. */
  crankerRefund?: PublicKey;
  lenderSeatIndexHint?: number | null;
  programId?: PublicKey;
};

/** Compose `ClaimRepayment` for a wallet-lender. Risk-profile lenders must
 *  use `claimRepaymentForRiskProfileIxs` instead. */
export function claimRepaymentIxs(args: ClaimRepaymentComposeArgs): TransactionInstruction[] {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return [
    createClaimRepaymentInstruction(
      {
        lender: args.lender,
        market: args.market.address,
        sequence: args.loan.data().matchedLoanSequence,
        debtBank: args.debtBank,
        marginfiProgram: args.marginfiProgram,
        crankerRefund: args.crankerRefund,
      },
      { lenderSeatIndexHint: args.lenderSeatIndexHint ?? null },
      programId,
    ),
  ];
}

export type SettlementComposeArgs = {
  market: Market;
  loan: Loan;
  payer: PublicKey;
  /** `0` (or absent) → repay full outstanding. */
  repayAtomsMax?: BN | bigint | number;
  liquidatorDebtToken: PublicKey;
  liquidatorCollateralToken: PublicKey;
  debtBank: PublicKey;
  collateralBank: PublicKey;
  debtLiquidityVault: PublicKey;
  collateralLiquidityVault: PublicKey;
  collateralBankLva: PublicKey;
  debtOracles: PublicKey[];
  collateralOracles: PublicKey[];
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
  tokenProgram?: PublicKey;
  programId?: PublicKey;
};

/** Compose `SettleMaturedLoan`. Permissionless keeper ix. Atoms land on
 *  `market.lender_integration_account`; lender drains separately. */
export function settleMaturedLoanIxs(args: SettlementComposeArgs): TransactionInstruction[] {
  return [buildSettlementIx(args, /*isLiquidate=*/ false)];
}

/** Compose `LiquidateLoan` for an unhealthy `Active` loan past its LTV
 *  threshold. Same accounts as `settleMaturedLoanIxs`. */
export function liquidateLoanIxs(args: SettlementComposeArgs): TransactionInstruction[] {
  return [buildSettlementIx(args, /*isLiquidate=*/ true)];
}

function buildSettlementIx(args: SettlementComposeArgs, isLiquidate: boolean): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  const builder = isLiquidate
    ? createLiquidateLoanInstruction
    : createSettleMaturedLoanInstruction;
  return builder(
    {
      payer: args.payer,
      market: args.market.address,
      sequence: args.loan.data().matchedLoanSequence,
      debtMint: args.market.debtMint(),
      collateralMint: args.market.collateralMint(),
      liquidatorDebtToken: args.liquidatorDebtToken,
      liquidatorCollateralToken: args.liquidatorCollateralToken,
      debtBank: args.debtBank,
      collateralBank: args.collateralBank,
      debtLiquidityVault: args.debtLiquidityVault,
      collateralLiquidityVault: args.collateralLiquidityVault,
      collateralBankLva: args.collateralBankLva,
      debtOracles: args.debtOracles,
      collateralOracles: args.collateralOracles,
      tokenProgram: args.tokenProgram ?? TOKEN_PROGRAM_ID,
      marginfiGroup: args.marginfiGroup,
      marginfiProgram: args.marginfiProgram,
    },
    { repayAtomsMax: toBN(args.repayAtomsMax ?? 0) },
    programId,
  );
}

export type ClaimRepaymentForRiskProfileComposeArgs = {
  market: Market;
  loan: Loan;
  payer: PublicKey;
  /** Lender vault PDA. When omitted, derived from `market.debtMint()`. */
  globalVault?: PublicKey;
  debtBank: PublicKey;
  debtLiquidityVault: PublicKey;
  debtBankLva: PublicKey;
  /** Single bank oracle — `debt_bank.config.primary_oracle()`. */
  bankOracle: PublicKey;
  lenderMarginfiAccount: PublicKey;
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
  tokenProgram?: PublicKey;
  /** Pass `loan.data().createdBy` to refund the registering cranker. */
  crankerRefund?: PublicKey;
  programId?: PublicKey;
};

/** Compose `ClaimRepaymentForRiskProfile`. Drains a vault-funded loan's
 *  repaid atoms from `market.lender_integration_account` back into the
 *  GlobalVault's marginfi account. */
export function claimRepaymentForRiskProfileIxs(
  args: ClaimRepaymentForRiskProfileComposeArgs,
): TransactionInstruction[] {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  const debtMint = args.market.debtMint();
  const globalVault = args.globalVault ?? globalVaultPda(debtMint, programId)[0];
  return [
    createClaimRepaymentForRiskProfileInstruction(
      {
        payer: args.payer,
        market: args.market.address,
        sequence: args.loan.data().matchedLoanSequence,
        globalVault,
        debtMint,
        debtBank: args.debtBank,
        debtLiquidityVault: args.debtLiquidityVault,
        debtBankLva: args.debtBankLva,
        bankOracle: args.bankOracle,
        lenderMarginfiAccount: args.lenderMarginfiAccount,
        tokenProgram: args.tokenProgram ?? TOKEN_PROGRAM_ID,
        marginfiGroup: args.marginfiGroup,
        marginfiProgram: args.marginfiProgram,
        crankerRefund: args.crankerRefund,
      },
      programId,
    ),
  ];
}

// ────────────────────── Protocol fee / P2Pool conversion ──────────────────────

export type ProtocolFeeClaimComposeArgs = {
  market: Market;
  /** Must equal `GlobalConfig.protocol_admin`. */
  protocolAdmin: PublicKey;
  /** Debt-token ATA owned by `protocolAdmin`. */
  protocolAdminDebtToken: PublicKey;
  debtBank: PublicKey;
  debtLiquidityVault: PublicKey;
  debtBankLva: PublicKey;
  debtOracles: PublicKey[];
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
  tokenProgram?: PublicKey;
  programId?: PublicKey;
};

/** Compose `ProtocolFeeClaim`. Signed by `GlobalConfig.protocol_admin`. */
export function protocolFeeClaimIxs(args: ProtocolFeeClaimComposeArgs): TransactionInstruction[] {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return [
    createProtocolFeeClaimInstruction(
      {
        protocolAdmin: args.protocolAdmin,
        market: args.market.address,
        debtMint: args.market.debtMint(),
        protocolAdminDebtToken: args.protocolAdminDebtToken,
        debtBank: args.debtBank,
        debtLiquidityVault: args.debtLiquidityVault,
        debtBankLva: args.debtBankLva,
        debtOracles: args.debtOracles,
        tokenProgram: args.tokenProgram ?? TOKEN_PROGRAM_ID,
        marginfiGroup: args.marginfiGroup,
        marginfiProgram: args.marginfiProgram,
      },
      programId,
    ),
  ];
}

export type ConvertP2PoolToFixedComposeArgs = {
  market: Market;
  loan: Loan;
  borrower: PublicKey;
  maxAcceptableRateBps: number;
  debtBank: PublicKey;
  debtLiquidityVault: PublicKey;
  debtBankLva: PublicKey;
  debtOracles: PublicKey[];
  collateralBank: PublicKey;
  collateralOracles: PublicKey[];
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
  tokenProgram?: PublicKey;
  /** Pass `loan.data().createdBy` to refund the originating cranker. */
  crankerRefund?: PublicKey;
  programId?: PublicKey;
};

/** Compose `ConvertP2PoolToFixed`. Borrower-signed; aborts if the live
 *  best-ask rate exceeds `maxAcceptableRateBps`. */
export function convertP2PoolToFixedIxs(
  args: ConvertP2PoolToFixedComposeArgs,
): TransactionInstruction[] {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return [
    createConvertP2PoolToFixedInstruction(
      {
        borrower: args.borrower,
        market: args.market.address,
        loanSequence: args.loan.data().matchedLoanSequence,
        debtMint: args.market.debtMint(),
        debtBank: args.debtBank,
        debtLiquidityVault: args.debtLiquidityVault,
        debtBankLva: args.debtBankLva,
        debtOracles: args.debtOracles,
        collateralBank: args.collateralBank,
        collateralOracles: args.collateralOracles,
        tokenProgram: args.tokenProgram ?? TOKEN_PROGRAM_ID,
        marginfiGroup: args.marginfiGroup,
        marginfiProgram: args.marginfiProgram,
        crankerRefund: args.crankerRefund,
      },
      { maxAcceptableRateBps: args.maxAcceptableRateBps },
      programId,
    ),
  ];
}

// ────────────────────── Pause toggles ──────────────────────

export type SetMarketPauseComposeArgs = {
  admin: PublicKey;
  market: Market | PublicKey;
  paused: boolean;
  programId?: PublicKey;
};

/** Compose `SetMarketPause`. Signed by `MarketFixed.admin` — the per-market
 *  admin set at CreateMarket time, not the protocol admin. */
export function setMarketPauseIx(args: SetMarketPauseComposeArgs): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  const market = args.market instanceof PublicKey ? args.market : args.market.address;
  return createSetMarketPauseInstruction(
    { admin: args.admin, market },
    { paused: args.paused },
    programId,
  );
}

export type SetGlobalPauseComposeArgs = {
  admin: PublicKey;
  paused: boolean;
  programId?: PublicKey;
};

/** Compose `SetGlobalPause`. Signed by `GlobalConfig.protocol_admin`. */
export function setGlobalPauseIx(args: SetGlobalPauseComposeArgs): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createSetGlobalPauseInstruction(
    { admin: args.admin },
    { paused: args.paused },
    programId,
  );
}

// ────────────────────── Global vault deposit / withdraw ──────────────────────

export type GlobalVaultDepositComposeArgs = {
  connection: Connection;
  depositor: PublicKey;
  mint: PublicKey;
  amountAtoms: BN | bigint | number;
  profileId: number;
  marginfiGroup: PublicKey;
  lendingPool: PublicKey;
  liquidityVault: PublicKey;
  marginfiProgram: PublicKey;
  tokenProgram?: PublicKey;
  /** Optional pre-resolved depositor ATA; otherwise auto-derived with a
   *  `createATA` ix prepended when missing. */
  depositorToken?: PublicKey;
  programId?: PublicKey;
};

/** Compose `[createATA?, GlobalVaultDeposit]`. */
export async function globalVaultDepositIxs(
  args: GlobalVaultDepositComposeArgs,
): Promise<TransactionInstruction[]> {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  const tokenProgram = args.tokenProgram ?? TOKEN_PROGRAM_ID;
  const ixs: TransactionInstruction[] = [];
  let depositorToken = args.depositorToken;
  if (!depositorToken) {
    const { ata, createIx } = await createAtaIfMissing(
      args.connection,
      args.depositor,
      args.depositor,
      args.mint,
      tokenProgram,
    );
    depositorToken = ata;
    if (createIx) ixs.push(createIx);
  }
  ixs.push(
    createGlobalVaultDepositInstruction(
      {
        depositor: args.depositor,
        mint: args.mint,
        depositorToken,
        tokenProgram,
        marginfiGroup: args.marginfiGroup,
        lendingPool: args.lendingPool,
        liquidityVault: args.liquidityVault,
        marginfiProgram: args.marginfiProgram,
      },
      { amountAtoms: toBN(args.amountAtoms), profileId: args.profileId },
      programId,
    ),
  );
  return ixs;
}

export type GlobalVaultWithdrawComposeArgs = {
  connection: Connection;
  depositor: PublicKey;
  mint: PublicKey;
  sharesToBurn: BN | bigint | number;
  profileId: number;
  marginfiGroup: PublicKey;
  lendingPool: PublicKey;
  lendingPoolOracle: PublicKey;
  liquidityVault: PublicKey;
  bankLiquidityVaultAuthority: PublicKey;
  marginfiProgram: PublicKey;
  tokenProgram?: PublicKey;
  depositorToken?: PublicKey;
  programId?: PublicKey;
};

/** Compose `[createATA?, GlobalVaultWithdraw]`. */
export async function globalVaultWithdrawIxs(
  args: GlobalVaultWithdrawComposeArgs,
): Promise<TransactionInstruction[]> {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  const tokenProgram = args.tokenProgram ?? TOKEN_PROGRAM_ID;
  const ixs: TransactionInstruction[] = [];
  let depositorToken = args.depositorToken;
  if (!depositorToken) {
    const { ata, createIx } = await createAtaIfMissing(
      args.connection,
      args.depositor,
      args.depositor,
      args.mint,
      tokenProgram,
    );
    depositorToken = ata;
    if (createIx) ixs.push(createIx);
  }
  ixs.push(
    createGlobalVaultWithdrawInstruction(
      {
        depositor: args.depositor,
        mint: args.mint,
        depositorToken,
        tokenProgram,
        marginfiGroup: args.marginfiGroup,
        lendingPool: args.lendingPool,
        lendingPoolOracle: args.lendingPoolOracle,
        liquidityVault: args.liquidityVault,
        bankLiquidityVaultAuthority: args.bankLiquidityVaultAuthority,
        marginfiProgram: args.marginfiProgram,
      },
      { sharesToBurn: toBN(args.sharesToBurn), profileId: args.profileId },
      programId,
    ),
  );
  return ixs;
}

// ────────────────────── Risk-profile market-side ixs ──────────────────────

export type RiskProfileMarketIxArgs = {
  payer: PublicKey;
  market: Market | PublicKey;
  /** Vault debt mint — derives the GlobalVault PDA. */
  mint: PublicKey;
  profileId: number;
  programId?: PublicKey;
};

function marketKey(m: Market | PublicKey): PublicKey {
  return m instanceof PublicKey ? m : m.address;
}

/** Compose `ClaimSeatForRiskProfile`. Carves out `maxExposureAtoms` on a
 *  market for the given `profileId`. */
export function claimSeatForRiskProfileIx(
  args: RiskProfileMarketIxArgs & { maxExposureAtoms: BN | bigint | number },
): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createClaimSeatForRiskProfileInstruction(
    { payer: args.payer, mint: args.mint, market: marketKey(args.market) },
    {
      profileId: args.profileId,
      maxExposureAtoms: toBN(args.maxExposureAtoms),
    },
    programId,
  );
}

/** Compose `ReleaseSeatForRiskProfile`. Frees the per-market seat after
 *  the curator has decommissioned the profile on that market. */
export function releaseSeatForRiskProfileIx(args: RiskProfileMarketIxArgs): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createReleaseSeatForRiskProfileInstruction(
    { payer: args.payer, mint: args.mint, market: marketKey(args.market) },
    { profileId: args.profileId },
    programId,
  );
}

export type SyncMarketSeatsForRiskProfileComposeArgs = {
  payer: PublicKey;
  /** Vault debt mint — derives the GlobalVault PDA. */
  debtMint: PublicKey;
  /** Up to 8 markets that hold a vault-side seat for this profile. */
  markets: PublicKey[];
  profileId: number;
  programId?: PublicKey;
};

/** Compose `SyncMarketSeatsForRiskProfile`. Bulk-refreshes vault-side
 *  seat snapshots across up to eight markets. */
export function syncMarketSeatsForRiskProfileIx(
  args: SyncMarketSeatsForRiskProfileComposeArgs,
): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createSyncMarketSeatsForRiskProfileInstruction(
    { payer: args.payer, debtMint: args.debtMint, markets: args.markets },
    { profileId: args.profileId },
    programId,
  );
}

export type PlaceOrderForRiskProfileComposeArgs = RiskProfileMarketIxArgs & {
  rateBps: number;
  termSeconds: number;
  flags?: number;
};

/** Compose `PlaceOrderForRiskProfile`. Posts an open-ended vault ask for
 *  the profile's idle capacity on `market`. */
export function placeOrderForRiskProfileIx(
  args: PlaceOrderForRiskProfileComposeArgs,
): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createPlaceOrderForRiskProfileInstruction(
    { payer: args.payer, mint: args.mint, market: marketKey(args.market) },
    {
      profileId: args.profileId,
      rateBps: args.rateBps,
      termSeconds: args.termSeconds,
      flags: args.flags ?? 0,
    },
    programId,
  );
}

/** Compose `CancelOrderForRiskProfile`. Removes the profile's resting
 *  vault ask on `market`. */
export function cancelOrderForRiskProfileIx(args: RiskProfileMarketIxArgs): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createCancelOrderForRiskProfileInstruction(
    { payer: args.payer, mint: args.mint, market: marketKey(args.market) },
    { profileId: args.profileId },
    programId,
  );
}

export type UpdateOrderForRiskProfileComposeArgs = RiskProfileMarketIxArgs & {
  newRateBps: number;
  newTermSeconds: number;
  newFlags?: number;
};

/** Compose `UpdateOrderForRiskProfile`. Replaces rate/term/flags on the
 *  profile's resting vault ask. */
export function updateOrderForRiskProfileIx(
  args: UpdateOrderForRiskProfileComposeArgs,
): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createUpdateOrderForRiskProfileInstruction(
    { payer: args.payer, mint: args.mint, market: marketKey(args.market) },
    {
      profileId: args.profileId,
      newRateBps: args.newRateBps,
      newTermSeconds: args.newTermSeconds,
      newFlags: args.newFlags ?? 0,
    },
    programId,
  );
}

// ────────────────────── Risk-profile lifecycle / curator ──────────────────────

export type CreateRiskProfileComposeArgs = {
  payer: PublicKey;
  mint: PublicKey;
  profileId: number;
  curator: PublicKey;
  maxLtvBps: number;
  maxTermSeconds: number;
  allowedMarketMax: number;
  programId?: PublicKey;
};

/** Compose `CreateRiskProfile`. Gated by `GlobalVaultFixed.global_vault_admin`
 *  (the per-vault admin set at CreateVault time). */
export function createRiskProfileIx(args: CreateRiskProfileComposeArgs): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createCreateRiskProfileInstruction(
    { payer: args.payer, mint: args.mint },
    {
      profileId: args.profileId,
      curator: args.curator,
      maxLtvBps: args.maxLtvBps,
      maxTermSeconds: args.maxTermSeconds,
      allowedMarketMax: args.allowedMarketMax,
    },
    programId,
  );
}

export type UpdateRiskProfileComposeArgs = {
  payer: PublicKey;
  mint: PublicKey;
  profileId: number;
  /** When `null`/absent, the field is left unchanged. */
  newMaxLtvBps?: number | null;
  newMaxTermSeconds?: number | null;
  newAllowedMarketMax?: number | null;
  programId?: PublicKey;
};

/** Compose `UpdateRiskProfile`. Vault-admin-signed; mutates a profile's
 *  caps (each Option<T> field is `null` to leave the existing value). */
export function updateRiskProfileIx(args: UpdateRiskProfileComposeArgs): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createUpdateRiskProfileInstruction(
    { payer: args.payer, mint: args.mint },
    {
      profileId: args.profileId,
      newMaxLtvBps: args.newMaxLtvBps ?? null,
      newMaxTermSeconds: args.newMaxTermSeconds ?? null,
      newAllowedMarketMax: args.newAllowedMarketMax ?? null,
    },
    programId,
  );
}

export type ClaimCuratorFeeComposeArgs = {
  payer: PublicKey;
  mint: PublicKey;
  profileId: number;
  curatorToken: PublicKey;
  debtBank: PublicKey;
  liquidityVault: PublicKey;
  bankLiquidityVaultAuthority: PublicKey;
  bankOracle: PublicKey;
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
  tokenProgram?: PublicKey;
  programId?: PublicKey;
};

/** Compose `ClaimCuratorFee`. Drains the profile's accumulated curator
 *  fee atoms from the GlobalVault into the curator's ATA. */
export function claimCuratorFeeIxs(args: ClaimCuratorFeeComposeArgs): TransactionInstruction[] {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return [
    createClaimCuratorFeeInstruction(
      {
        payer: args.payer,
        mint: args.mint,
        curatorToken: args.curatorToken,
        debtBank: args.debtBank,
        liquidityVault: args.liquidityVault,
        bankLiquidityVaultAuthority: args.bankLiquidityVaultAuthority,
        bankOracle: args.bankOracle,
        tokenProgram: args.tokenProgram ?? TOKEN_PROGRAM_ID,
        marginfiProgram: args.marginfiProgram,
        marginfiGroup: args.marginfiGroup,
      },
      { profileId: args.profileId },
      programId,
    ),
  ];
}

// ────────────────────── Admin-transfer gate-keepers ──────────────────────

export type TransferMarketAdminComposeArgs = {
  currentAdmin: PublicKey;
  market: Market | PublicKey;
  newAdmin: PublicKey;
  programId?: PublicKey;
};

/** Compose `TransferMarketAdmin`. Two-step handoff — `AcceptMarketAdmin`
 *  must follow before the new key is live. */
export function transferMarketAdminIx(args: TransferMarketAdminComposeArgs): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createTransferMarketAdminInstruction(
    { currentAdmin: args.currentAdmin, market: marketKey(args.market) },
    { newAdmin: args.newAdmin },
    programId,
  );
}

export type AcceptMarketAdminComposeArgs = {
  pendingAdmin: PublicKey;
  market: Market | PublicKey;
  programId?: PublicKey;
};

/** Compose `AcceptMarketAdmin`. Promotes `pending_market_admin` to live. */
export function acceptMarketAdminIx(args: AcceptMarketAdminComposeArgs): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createAcceptMarketAdminInstruction(
    { pendingAdmin: args.pendingAdmin, market: marketKey(args.market) },
    programId,
  );
}

export type TransferGlobalVaultAdminComposeArgs = {
  currentAdmin: PublicKey;
  mint: PublicKey;
  newAdmin: PublicKey;
  programId?: PublicKey;
};

/** Compose `TransferGlobalVaultAdmin`. Two-step handoff for a per-mint
 *  GlobalVault admin. */
export function transferGlobalVaultAdminIx(
  args: TransferGlobalVaultAdminComposeArgs,
): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createTransferGlobalVaultAdminInstruction(
    { currentAdmin: args.currentAdmin, mint: args.mint },
    { newAdmin: args.newAdmin },
    programId,
  );
}

export type AcceptGlobalVaultAdminComposeArgs = {
  pendingAdmin: PublicKey;
  mint: PublicKey;
  programId?: PublicKey;
};

/** Compose `AcceptGlobalVaultAdmin`. Promotes the pending GlobalVault admin. */
export function acceptGlobalVaultAdminIx(
  args: AcceptGlobalVaultAdminComposeArgs,
): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createAcceptGlobalVaultAdminInstruction(
    { pendingAdmin: args.pendingAdmin, mint: args.mint },
    programId,
  );
}

export type TransferCuratorComposeArgs = {
  currentCurator: PublicKey;
  mint: PublicKey;
  profileId: number;
  newCurator: PublicKey;
  programId?: PublicKey;
};

/** Compose `TransferCurator`. Two-step handoff for the profile curator. */
export function transferCuratorIx(args: TransferCuratorComposeArgs): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createTransferCuratorInstruction(
    { currentCurator: args.currentCurator, mint: args.mint },
    { profileId: args.profileId, newCurator: args.newCurator },
    programId,
  );
}

export type AcceptCuratorComposeArgs = {
  pendingCurator: PublicKey;
  mint: PublicKey;
  profileId: number;
  programId?: PublicKey;
};

/** Compose `AcceptCurator`. Promotes the pending curator on a profile. */
export function acceptCuratorIx(args: AcceptCuratorComposeArgs): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createAcceptCuratorInstruction(
    { pendingCurator: args.pendingCurator, mint: args.mint },
    { profileId: args.profileId },
    programId,
  );
}

export type TransferProtocolAdminComposeArgs = {
  currentAdmin: PublicKey;
  newAdmin: PublicKey;
  programId?: PublicKey;
};

/** Compose `TransferProtocolAdmin`. Two-step handoff for the
 *  protocol-wide `GlobalConfig.protocol_admin`. */
export function transferProtocolAdminIx(
  args: TransferProtocolAdminComposeArgs,
): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createTransferProtocolAdminInstruction(
    { currentAdmin: args.currentAdmin },
    { newAdmin: args.newAdmin },
    programId,
  );
}

export type AcceptProtocolAdminComposeArgs = {
  pendingAdmin: PublicKey;
  programId?: PublicKey;
};

/** Compose `AcceptProtocolAdmin`. Promotes the pending protocol admin. */
export function acceptProtocolAdminIx(
  args: AcceptProtocolAdminComposeArgs,
): TransactionInstruction {
  const programId = args.programId ?? YDELTA_PROGRAM_ID;
  return createAcceptProtocolAdminInstruction(
    { pendingAdmin: args.pendingAdmin },
    programId,
  );
}
