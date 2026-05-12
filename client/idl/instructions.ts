import { IdlInstruction } from './format';

/** Convenience constructors for the recurring account shapes — keeps the
 *  instruction definitions readable. */
const writableSigner = (name: string, docs?: string): IdlInstruction['accounts'][number] => ({
  name,
  isMut: true,
  isSigner: true,
  ...(docs ? { docs: [docs] } : {}),
});
/** Signer that contributes its signature but isn't debited. Used by
 *  the split-payer risk-profile ixs where the curator authorises but
 *  the fee_payer carries any lamport flow. */
const readonlySigner = (name: string, docs?: string): IdlInstruction['accounts'][number] => ({
  name,
  isMut: false,
  isSigner: true,
  ...(docs ? { docs: [docs] } : {}),
});
const writable = (name: string, docs?: string): IdlInstruction['accounts'][number] => ({
  name,
  isMut: true,
  isSigner: false,
  ...(docs ? { docs: [docs] } : {}),
});
const ro = (name: string, docs?: string): IdlInstruction['accounts'][number] => ({
  name,
  isMut: false,
  isSigner: false,
  ...(docs ? { docs: [docs] } : {}),
});

const GLOBAL_CONFIG = ro('globalConfig', 'PDA `[b"global_config"]` — gates every state-mutating ix.');
const SYSTEM_PROGRAM = ro('systemProgram');
const MARGINFI_PROGRAM = ro('marginfiProgram');
const MARGINFI_GROUP = ro('marginfiGroup');
const TOKEN_PROGRAM = ro('tokenProgram');

/** Program instructions, in tag order. */
export const instructions: IdlInstruction[] = [
  // ─── tag 0 ───
  {
    name: 'CreateMarket',
    discriminant: { type: 'u8', value: 0 },
    docs: [
      'Create a `(debt_mint, collateral_mint)` lending market.',
      'Signer must equal `GlobalConfig.protocol_admin` (see CreateMarketContext).',
    ],
    args: [],
    accounts: [
      writableSigner('payer', 'Must equal `GlobalConfig.protocol_admin`.'),
      GLOBAL_CONFIG,
      writable('market', 'Caller-supplied market keypair; becomes PDA seed for downstream PDAs.'),
      SYSTEM_PROGRAM,
      ro('debtMint'),
      ro('collateralMint'),
      writable('debtVault', 'PDA `[b"vault", market, debt_mint]`.'),
      writable('collateralVault', 'PDA `[b"vault", market, collateral_mint]`.'),
      ro('tokenProgram'),
      ro('tokenProgram22'),
      MARGINFI_GROUP,
      ro('debtBank'),
      ro('collateralBank'),
      writable('lenderMarginfiAccount', 'PDA `[b"marginfi_account", market]`; created via CPI.'),
      writable('borrowerMarginfiAccount', 'PDA `[b"borrower_marginfi_account", market]`; created via CPI.'),
      ro('marketSigner', 'PDA `[b"market_signer", market]`.'),
      MARGINFI_PROGRAM,
    ],
  },

  // ─── tag 1 ───
  {
    name: 'ClaimSeat',
    discriminant: { type: 'u8', value: 1 },
    docs: ['Allocate the signer\'s `ClaimedSeat` in `market`. Permissionless.'],
    args: [],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('market'),
      SYSTEM_PROGRAM,
      writable('userAccount', 'PDA `[b"user", payer]`; auto-created on first claim.'),
    ],
  },

  // ─── tag 2 ───
  {
    name: 'Deposit',
    discriminant: { type: 'u8', value: 2 },
    docs: ['Deposit debt-mint or collateral-mint atoms into the signer\'s seat.'],
    args: [{ name: 'params', type: { defined: 'DepositParams' } }],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('market'),
      writable('traderToken'),
      writable('vault', 'Market staging vault PDA `[b"vault", market, mint]`.'),
      TOKEN_PROGRAM,
      ro('mint'),
      MARGINFI_GROUP,
      writable('marginfiAccount', 'Lender or borrower marginfi account, side-determined.'),
      writable('bank'),
      writable('liquidityVault'),
      ro('marketSigner'),
      MARGINFI_PROGRAM,
      writable('userAccount'),
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 3 ───
  {
    name: 'Withdraw',
    discriminant: { type: 'u8', value: 3 },
    docs: ['Withdraw withdrawable shares back to a wallet ATA.'],
    args: [{ name: 'params', type: { defined: 'WithdrawParams' } }],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('market'),
      writable('traderToken'),
      writable('vault'),
      TOKEN_PROGRAM,
      ro('mint'),
      MARGINFI_GROUP,
      writable('marginfiAccount'),
      writable('debtBank'),
      writable('collateralBank'),
      writable('liquidityVault'),
      ro('bankLiquidityVaultAuthority'),
      ro('debtOracle', 'First entry of the variadic debt-oracle slice (PythPush = 1 acct, Staked = 3).'),
      ro('collateralOracle', 'First entry of the variadic collateral-oracle slice.'),
      ro('marketSigner'),
      MARGINFI_PROGRAM,
      writable('userAccount'),
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 4 ───
  {
    name: 'PlaceOrder',
    discriminant: { type: 'u8', value: 4 },
    docs: ['Place a primary order or secondary loan-sale bid.'],
    args: [{ name: 'params', type: { defined: 'PlaceOrderParams' } }],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('market'),
      SYSTEM_PROGRAM,
      MARGINFI_GROUP,
      writable('borrowerMarginfiAccount'),
      writable('debtBank'),
      writable('collateralBank'),
      ro('debtOracle', 'First entry of the variadic debt-oracle slice (PythPush = 1 acct, Staked = 3).'),
      ro('collateralOracle', 'First entry of the variadic collateral-oracle slice.'),
      ro('marketSigner'),
      MARGINFI_PROGRAM,
      writable('debtLiquidityVault'),
      ro('debtBankLiquidityVaultAuthority'),
      writable('borrowerDebtToken'),
      TOKEN_PROGRAM,
      writable('userAccount'),
      SYSTEM_PROGRAM,
      writable('lenderMarginfiAccount', 'For P2Pool deposit-back path on Bids; also the route for the borrower-LTV check.'),
      writable('marketDebtVault'),
      writable('secondaryLoan', 'Optional — only for `OrderKind::SecondaryLoanSale`.'),
      writable('globalVault', 'Vault PDA derived from `debt_mint`; reads/mutates vault makers in match.'),
    ],
  },

  // ─── tag 5 ───
  {
    name: 'CancelOrder',
    discriminant: { type: 'u8', value: 5 },
    docs: ['Cancel an open order owned by the signer.'],
    args: [{ name: 'params', type: { defined: 'CancelOrderParams' } }],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('market'),
      SYSTEM_PROGRAM,
      writable('userAccount'),
      SYSTEM_PROGRAM,
      writable('secondaryLoan', 'Optional — required for SecondaryLoanSale cancels.'),
    ],
  },

  // ─── tag 6 ───
  {
    name: 'UpdateOrder',
    discriminant: { type: 'u8', value: 6 },
    docs: ['Update an open order — cancel-and-replace; renews sequence.'],
    args: [{ name: 'params', type: { defined: 'UpdateOrderParams' } }],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('market'),
      SYSTEM_PROGRAM,
      writable('userAccount'),
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 7 ───
  {
    name: 'ProcessMatchedLoan',
    discriminant: { type: 'u8', value: 7 },
    docs: ['Promote a `MatchedLoan` queue entry into a `LoanFixed` PDA. Permissionless cranker.'],
    args: [{ name: 'params', type: { defined: 'ProcessMatchedLoanParams' } }],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('market'),
      writable('loan', 'Loan PDA `[b"loan", market, sequence]`; allocated on primary.'),
      writable('debtBank'),
      MARGINFI_PROGRAM,
      SYSTEM_PROGRAM,
      ro('collateralBank', 'Secondary only — omit for primary.'),
      ro('debtOracle', 'Secondary only. First entry of the variadic debt oracle slice (PythPush = 1 acct, Staked = 3).'),
      ro('collateralOracle', 'Secondary only. First entry of the variadic collateral oracle slice.'),
      writable('splitLoan', 'SECONDARY|SPLIT only — fresh PDA `[b"split_loan", market, queue_seq]`.'),
      writable('globalVault', 'Optional — appended when trigger seat is risk-profile-owned.'),
      ro('globalVaultSigner', 'Optional.'),
      writable('globalVaultStaging', 'Optional.'),
      writable('globalVaultIntegrationAccount', 'Optional.'),
      writable('marketDebtVault', 'Optional.'),
      writable('marketLenderIntegrationAccount', 'Optional.'),
      ro('marketSigner', 'Optional.'),
      writable('debtLiquidityVault', 'Optional.'),
      ro('debtBankLiquidityVaultAuthority', 'Optional.'),
      ro('bankOracle', 'Optional — `debt_bank.config.oracle_keys[0]` only (vault-settle is not variadic).'),
      ro('debtMint', 'Optional.'),
      ro('tokenProgramForVault', 'Optional — for vault-settle CPI.'),
      ro('marginfiGroupForVault', 'Optional.'),
      ro('marginfiProgramForVault', 'Optional.'),
    ],
  },

  // ─── tag 8 ───
  {
    name: 'Repay',
    discriminant: { type: 'u8', value: 8 },
    docs: ['Borrower repays principal + interest on an active loan.'],
    args: [{ name: 'params', type: { defined: 'RepayParams' } }],
    accounts: [
      writableSigner('borrower'),
      GLOBAL_CONFIG,
      writable('market'),
      writable('loan'),
      writable('borrowerToken'),
      writable('vault', 'Market staging vault PDA `[b"vault", market, debt_mint]`.'),
      TOKEN_PROGRAM,
      ro('debtMint'),
      MARGINFI_GROUP,
      writable('lenderMarginfiAccount'),
      writable('debtBank'),
      writable('debtLiquidityVault'),
      ro('collateralBank'),
      ro('marketSigner'),
      MARGINFI_PROGRAM,
      writable('userAccount'),
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 9 ───
  {
    name: 'ClaimRepayment',
    discriminant: { type: 'u8', value: 9 },
    docs: ['Wallet-lender claims accrued atoms from a repaid loan.'],
    args: [{ name: 'params', type: { defined: 'ClaimRepaymentParams' } }],
    accounts: [
      writableSigner('lender'),
      GLOBAL_CONFIG,
      writable('market'),
      writable('loan'),
      ro('debtBank'),
      MARGINFI_PROGRAM,
      writable('userAccount'),
      SYSTEM_PROGRAM,
      writable('crankerRefund', 'Optional — receives the loan PDA rent refund on full settle.'),
    ],
  },

  // ─── tag 10 ───
  {
    name: 'SyncMarketPosition',
    discriminant: { type: 'u8', value: 10 },
    docs: ['Permissionless: copy canonical seat balances into the owner\'s mirror.'],
    args: [],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('ownerUserAccount'),
      ro('market'),
      ro('owner'),
    ],
  },

  // ─── tag 11 ───
  {
    name: 'CreateVault',
    discriminant: { type: 'u8', value: 11 },
    docs: ['Allocate the per-mint `GlobalVault` + init marginfi integration account.'],
    args: [],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('vault'),
      ro('mint'),
      ro('globalVaultSigner'),
      writable('integrationAccount'),
      writable('globalVaultStaging'),
      TOKEN_PROGRAM,
      ro('tokenProgram22'),
      MARGINFI_GROUP,
      ro('lendingPool'),
      MARGINFI_PROGRAM,
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 12 ───
  {
    name: 'CreateRiskProfile',
    discriminant: { type: 'u8', value: 12 },
    docs: [
      "Insert a `RiskProfile` into the vault's tree.",
      'Signer must equal `GlobalVaultFixed.global_vault_admin`.',
    ],
    args: [{ name: 'params', type: { defined: 'CreateRiskProfileParams' } }],
    accounts: [
      writableSigner('payer', 'Must equal `GlobalVaultFixed.global_vault_admin`.'),
      GLOBAL_CONFIG,
      writable('vault'),
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 13 ───
  {
    name: 'GlobalVaultDeposit',
    discriminant: { type: 'u8', value: 13 },
    docs: ['Depositor mints vault-profile shares.'],
    args: [{ name: 'params', type: { defined: 'GlobalVaultDepositParams' } }],
    accounts: [
      writableSigner('depositor'),
      GLOBAL_CONFIG,
      writable('vault'),
      ro('mint'),
      ro('globalVaultSigner'),
      writable('globalVaultStaging'),
      writable('depositorToken'),
      TOKEN_PROGRAM,
      MARGINFI_GROUP,
      writable('integrationAccount'),
      writable('lendingPool'),
      writable('liquidityVault'),
      MARGINFI_PROGRAM,
      writable('userAccount'),
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 14 ───
  {
    name: 'GlobalVaultWithdraw',
    discriminant: { type: 'u8', value: 14 },
    docs: ['Depositor burns shares to redeem atoms; reverts on deployed-not-available.'],
    args: [{ name: 'params', type: { defined: 'GlobalVaultWithdrawParams' } }],
    accounts: [
      writableSigner('depositor'),
      GLOBAL_CONFIG,
      writable('vault'),
      ro('mint'),
      ro('globalVaultSigner'),
      writable('globalVaultStaging'),
      writable('depositorToken'),
      TOKEN_PROGRAM,
      MARGINFI_GROUP,
      writable('integrationAccount'),
      writable('lendingPool'),
      ro('lendingPoolOracle'),
      writable('liquidityVault'),
      ro('bankLiquidityVaultAuthority'),
      MARGINFI_PROGRAM,
      writable('userAccount'),
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 15 ───
  {
    name: 'ClaimSeatForRiskProfile',
    discriminant: { type: 'u8', value: 15 },
    docs: [
      "Curator opens a market seat for a profile with a per-market cap. Split-payer: feePayer covers tx fee + rent for the new seat block; curator only signs to satisfy the profile.curator gate.",
    ],
    args: [{ name: 'params', type: { defined: 'ClaimSeatForRiskProfileParams' } }],
    accounts: [
      writableSigner('feePayer', 'Tx fee payer + rent funder for market seat-block expansion.'),
      readonlySigner('curator', 'Must equal `RiskProfile.curator`. No lamports debited.'),
      GLOBAL_CONFIG,
      writable('vault'),
      writable('market'),
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 16 ───
  {
    name: 'PlaceOrderForRiskProfile',
    discriminant: { type: 'u8', value: 16 },
    docs: [
      "Curator places a vault Ask. Split-payer: feePayer covers tx fee + any vault node-block rent; curator only signs.",
    ],
    args: [{ name: 'params', type: { defined: 'PlaceOrderForRiskProfileParams' } }],
    accounts: [
      writableSigner('feePayer', 'Tx fee payer + rent funder for any node-block expansion.'),
      readonlySigner('curator', 'Must equal `RiskProfile.curator`.'),
      GLOBAL_CONFIG,
      writable('vault'),
      writable('market'),
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 17 ───
  {
    name: 'CancelOrderForRiskProfile',
    discriminant: { type: 'u8', value: 17 },
    docs: [
      "Curator cancels a vault order. Split-payer: feePayer covers tx fee (no expansion happens); curator only signs.",
    ],
    args: [{ name: 'params', type: { defined: 'CancelOrderForRiskProfileParams' } }],
    accounts: [
      writableSigner('feePayer', 'Tx fee payer.'),
      readonlySigner('curator', 'Must equal `RiskProfile.curator`.'),
      GLOBAL_CONFIG,
      writable('vault'),
      writable('market'),
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 18 ───
  {
    name: 'UpdateOrderForRiskProfile',
    discriminant: { type: 'u8', value: 18 },
    docs: [
      "Curator updates a vault order via cancel-and-replace. Split-payer: feePayer covers tx fee + any rent if expansion fires; curator only signs.",
    ],
    args: [{ name: 'params', type: { defined: 'UpdateOrderForRiskProfileParams' } }],
    accounts: [
      writableSigner('feePayer', 'Tx fee payer + rent funder for any node-block expansion.'),
      readonlySigner('curator', 'Must equal `RiskProfile.curator`.'),
      GLOBAL_CONFIG,
      writable('vault'),
      writable('market'),
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 19 ───
  {
    name: 'ClaimCuratorFee',
    discriminant: { type: 'u8', value: 19 },
    docs: ['Curator withdraws accrued fee atoms to a wallet ATA. Curator-gated.'],
    args: [{ name: 'params', type: { defined: 'ClaimCuratorFeeParams' } }],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('vault'),
      ro('globalVaultSigner'),
      writable('globalVaultStaging'),
      writable('vaultIntegration'),
      writable('curatorToken'),
      writable('debtBank'),
      writable('liquidityVault'),
      ro('bankLiquidityVaultAuthority'),
      ro('bankOracle'),
      ro('mint'),
      TOKEN_PROGRAM,
      MARGINFI_PROGRAM,
      MARGINFI_GROUP,
    ],
  },

  // ─── tag 20 ───
  {
    name: 'SettleMaturedLoan',
    discriminant: { type: 'u8', value: 20 },
    docs: ['Permissionless: settle a loan past `matures_at + grace`.'],
    args: [
      { name: 'repayAtomsMax', type: 'u64', docs: ['0 → repay full outstanding.'] },
    ],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('market'),
      writable('loan'),
      writable('liquidatorDebtToken'),
      writable('liquidatorCollateralToken'),
      writable('marketDebtVault'),
      writable('marketCollateralVault'),
      ro('marketSigner'),
      writable('lenderMarginfiAccount'),
      writable('borrowerMarginfiAccount'),
      writable('debtBank'),
      writable('collateralBank'),
      writable('debtLiquidityVault'),
      writable('collateralLiquidityVault'),
      ro('collateralBankLva'),
      ro('debtOracle', 'First entry of the variadic debt-oracle slice.'),
      ro('collateralOracle', 'First entry of the variadic collateral-oracle slice.'),
      ro('debtMint'),
      ro('collateralMint'),
      TOKEN_PROGRAM,
      MARGINFI_GROUP,
      MARGINFI_PROGRAM,
    ],
  },

  // ─── tag 21 ───
  {
    name: 'LiquidateLoan',
    discriminant: { type: 'u8', value: 21 },
    docs: ['Permissionless: liquidate a loan whose current LTV breaches maintenance.'],
    args: [
      { name: 'repayAtomsMax', type: 'u64', docs: ['0 → repay full outstanding.'] },
    ],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('market'),
      writable('loan'),
      writable('liquidatorDebtToken'),
      writable('liquidatorCollateralToken'),
      writable('marketDebtVault'),
      writable('marketCollateralVault'),
      ro('marketSigner'),
      writable('lenderMarginfiAccount'),
      writable('borrowerMarginfiAccount'),
      writable('debtBank'),
      writable('collateralBank'),
      writable('debtLiquidityVault'),
      writable('collateralLiquidityVault'),
      ro('collateralBankLva'),
      ro('debtOracle', 'First entry of the variadic debt-oracle slice.'),
      ro('collateralOracle', 'First entry of the variadic collateral-oracle slice.'),
      ro('debtMint'),
      ro('collateralMint'),
      TOKEN_PROGRAM,
      MARGINFI_GROUP,
      MARGINFI_PROGRAM,
    ],
  },

  // ─── tag 22 ───
  {
    name: 'SetFeeConfig',
    discriminant: { type: 'u8', value: 22 },
    docs: ['Update per-market `FeeConfig` fields. Admin-gated.'],
    args: [{ name: 'params', type: { defined: 'SetFeeConfigParams' } }],
    accounts: [
      writableSigner('admin'),
      GLOBAL_CONFIG,
      writable('market'),
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 23 ───
  {
    name: 'ProtocolFeeClaim',
    discriminant: { type: 'u8', value: 23 },
    docs: [
      "Drain accumulated protocol fee shares to protocol_admin's debt-token ATA.",
      'Signer must equal `GlobalConfig.protocol_admin` (NOT MarketFixed.admin).',
      'Gating on market.admin would let every per-market admin siphon protocol',
      'fees from their own market.',
    ],
    args: [],
    accounts: [
      writableSigner('protocolAdmin', 'Must equal `GlobalConfig.protocol_admin`.'),
      GLOBAL_CONFIG,
      writable('market'),
      writable('protocolAdminDebtToken'),
      writable('marketDebtVault'),
      ro('marketSigner'),
      writable('lenderMarginfiAccount'),
      writable('debtBank'),
      writable('debtLiquidityVault'),
      ro('debtBankLva'),
      ro('debtOracle', 'First entry of the variadic debt-oracle slice.'),
      ro('debtMint'),
      TOKEN_PROGRAM,
      MARGINFI_GROUP,
      MARGINFI_PROGRAM,
    ],
  },

  // ─── tag 24 ───
  {
    name: 'ClaimRepaymentForRiskProfile',
    discriminant: { type: 'u8', value: 24 },
    docs: [
      'Permissionless cranker: realise a fully-repaid risk-profile loan.',
      'The vault-settle loader reads exactly one bank_oracle',
      '(`bank.config.primary_oracle()`); not a variadic slice.',
    ],
    args: [],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('market'),
      writable('loan'),
      writable('globalVault'),
      ro('globalVaultSigner'),
      writable('globalVaultStaging'),
      writable('globalVaultIntegrationAccount'),
      writable('marketDebtVault'),
      ro('marketSigner'),
      writable('lenderMarginfiAccount'),
      writable('debtBank'),
      writable('debtLiquidityVault'),
      ro('debtBankLva'),
      ro('bankOracle', 'Single bank oracle = `debt_bank.config.primary_oracle()`.'),
      ro('debtMint'),
      TOKEN_PROGRAM,
      MARGINFI_GROUP,
      MARGINFI_PROGRAM,
      writable('crankerRefund', 'Optional.'),
    ],
  },

  // ─── tags 25–26: market admin two-step ───
  {
    name: 'TransferMarketAdmin',
    discriminant: { type: 'u8', value: 25 },
    docs: ['Initiate market admin transfer. Signer must equal current admin.'],
    args: [{ name: 'params', type: { defined: 'TransferAdminParams' } }],
    accounts: [
      writableSigner('currentAdmin'),
      GLOBAL_CONFIG,
      writable('market'),
    ],
  },
  {
    name: 'AcceptMarketAdmin',
    discriminant: { type: 'u8', value: 26 },
    docs: ['Finalise market admin transfer. Signer must equal pending_admin.'],
    args: [],
    accounts: [
      writableSigner('pendingAdmin'),
      GLOBAL_CONFIG,
      writable('market'),
    ],
  },

  // ─── tags 27–28: vault admin two-step ───
  {
    name: 'TransferGlobalVaultAdmin',
    discriminant: { type: 'u8', value: 27 },
    docs: ['Initiate vault admin transfer.'],
    args: [{ name: 'params', type: { defined: 'TransferAdminParams' } }],
    accounts: [
      writableSigner('currentAdmin'),
      GLOBAL_CONFIG,
      writable('vault'),
    ],
  },
  {
    name: 'AcceptGlobalVaultAdmin',
    discriminant: { type: 'u8', value: 28 },
    docs: ['Finalise vault admin transfer.'],
    args: [],
    accounts: [
      writableSigner('pendingAdmin'),
      GLOBAL_CONFIG,
      writable('vault'),
    ],
  },

  // ─── tags 29–30: curator two-step ───
  {
    name: 'TransferCurator',
    discriminant: { type: 'u8', value: 29 },
    docs: ['Initiate per-profile curator transfer.'],
    args: [{ name: 'params', type: { defined: 'TransferCuratorParams' } }],
    accounts: [
      writableSigner('currentCurator'),
      GLOBAL_CONFIG,
      writable('vault'),
    ],
  },
  {
    name: 'AcceptCurator',
    discriminant: { type: 'u8', value: 30 },
    docs: ['Finalise per-profile curator transfer.'],
    args: [{ name: 'params', type: { defined: 'AcceptCuratorParams' } }],
    accounts: [
      writableSigner('pendingCurator'),
      GLOBAL_CONFIG,
      writable('vault'),
    ],
  },

  // ─── tag 31 ───
  {
    name: 'SetMarketPause',
    discriminant: { type: 'u8', value: 31 },
    docs: ['Set MarketFixed.is_paused. Signer must equal `MarketFixed.admin`.'],
    args: [{ name: 'params', type: { defined: 'SetMarketPauseParams' } }],
    accounts: [
      writableSigner('admin', 'Must equal `MarketFixed.admin`.'),
      GLOBAL_CONFIG,
      writable('market'),
    ],
  },

  // ─── tag 32 ───
  {
    name: 'CreateGlobalConfig',
    discriminant: { type: 'u8', value: 32 },
    docs: ['One-shot. Allocates `[b"global_config"]`; signer must equal the BPF upgrade authority.'],
    args: [],
    accounts: [
      writableSigner('payer'),
      writable('globalConfig'),
      SYSTEM_PROGRAM,
      ro('programData', 'BpfLoaderUpgradeable::ProgramData PDA for the yDelta program.'),
    ],
  },

  // ─── tags 33–35: protocol admin lifecycle ───
  {
    name: 'TransferProtocolAdmin',
    discriminant: { type: 'u8', value: 33 },
    docs: ['Initiate protocol_admin transfer.'],
    args: [{ name: 'params', type: { defined: 'TransferAdminParams' } }],
    accounts: [
      writableSigner('currentAdmin'),
      writable('globalConfig'),
    ],
  },
  {
    name: 'AcceptProtocolAdmin',
    discriminant: { type: 'u8', value: 34 },
    docs: ['Finalise protocol_admin transfer.'],
    args: [],
    accounts: [
      writableSigner('pendingAdmin'),
      writable('globalConfig'),
    ],
  },
  {
    name: 'SetGlobalPause',
    discriminant: { type: 'u8', value: 35 },
    docs: ['Set GlobalConfig.is_paused. Protocol-admin-gated.'],
    args: [{ name: 'params', type: { defined: 'SetGlobalPauseParams' } }],
    accounts: [
      writableSigner('admin'),
      writable('globalConfig'),
    ],
  },

  // ─── tag 36 ───
  {
    name: 'UpdateRiskProfile',
    discriminant: { type: 'u8', value: 36 },
    docs: ['Mutate `RiskProfile` policy fields. Vault-admin-gated.'],
    args: [{ name: 'params', type: { defined: 'UpdateRiskProfileParams' } }],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('vault'),
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 37 ───
  {
    name: 'ReleaseSeatForRiskProfile',
    discriminant: { type: 'u8', value: 37 },
    docs: [
      "Tear down an empty vault-owned ClaimedSeat in a market. Curator-gated; split-payer: feePayer covers tx fee, curator only signs.",
    ],
    args: [{ name: 'params', type: { defined: 'ReleaseSeatForRiskProfileParams' } }],
    accounts: [
      writableSigner('feePayer', 'Tx fee payer.'),
      readonlySigner('curator', 'Must equal `RiskProfile.curator`.'),
      GLOBAL_CONFIG,
      writable('vault'),
      writable('market'),
      SYSTEM_PROGRAM,
    ],
  },

  // ─── tag 38 ───
  {
    name: 'SyncMarketSeatsForRiskProfile',
    discriminant: { type: 'u8', value: 38 },
    docs: ['Permissionless: re-stamp risk_profile_max_ltv_bps caches across up to 8 markets.'],
    args: [{ name: 'params', type: { defined: 'SyncMarketSeatsForRiskProfileParams' } }],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      ro('globalVault'),
      writable('market', 'Variadic — up to 8 markets in `RiskProfile.active_markets`.'),
    ],
  },

  // ─── tag 39 ───
  {
    name: 'ConvertP2PoolToFixed',
    discriminant: { type: 'u8', value: 39 },
    docs: ['Borrower-initiated refinance of a P2Pool loan into Fixed loans.'],
    args: [{ name: 'params', type: { defined: 'ConvertP2PoolToFixedParams' } }],
    accounts: [
      writableSigner('borrower'),
      GLOBAL_CONFIG,
      writable('market'),
      writable('loan'),
      writable('borrowerMarginfiAccount'),
      writable('lenderMarginfiAccount'),
      writable('debtBank'),
      writable('debtLiquidityVault'),
      ro('debtBankLva'),
      ro('debtOracle', 'First entry of the variadic debt-oracle slice.'),
      ro('collateralBank'),
      ro('collateralOracle', 'First entry of the variadic collateral-oracle slice.'),
      writable('marketDebtVault'),
      ro('debtMint'),
      ro('marketSigner'),
      TOKEN_PROGRAM,
      MARGINFI_GROUP,
      MARGINFI_PROGRAM,
      writable('crankerRefund', 'Optional.'),
    ],
  },

  // ─── tags 40–41: read-only simulation gates ───
  {
    name: 'CheckLtvLiquidatable',
    discriminant: { type: 'u8', value: 40 },
    docs: ['Read-only sim gate. Ok iff the loan would fail marginfi maint solvency.'],
    args: [],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('market'),
      writable('loan'),
      writable('borrowerMarginfiAccount'),
      ro('debtBank'),
      ro('debtOracle', 'First entry of the variadic debt-oracle slice.'),
      ro('collateralBank'),
      ro('collateralOracle', 'First entry of the variadic collateral-oracle slice.'),
      MARGINFI_PROGRAM,
    ],
  },
  {
    name: 'CheckMaturityLiquidatable',
    discriminant: { type: 'u8', value: 41 },
    docs: ['Read-only sim gate. Ok iff past `matures_at + grace` AND outstanding > 0.'],
    args: [],
    accounts: [
      writableSigner('payer'),
      GLOBAL_CONFIG,
      writable('market'),
      writable('loan'),
      writable('borrowerMarginfiAccount'),
      ro('debtBank'),
      MARGINFI_PROGRAM,
    ],
  },
];
