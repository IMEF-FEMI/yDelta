/** YdeltaError taxonomy — mirrors programs/ydelta/src/program/error.rs. */

type ErrorWithCode = Error & { code: number; name: string };
type MaybeErrorWithCode = ErrorWithCode | null;

const codeToCtor: Map<number, () => ErrorWithCode> = new Map();
const nameToCtor: Map<string, () => ErrorWithCode> = new Map();

function register(
  code: number,
  name: string,
  message: string,
): new () => ErrorWithCode {
  const cls = class extends Error {
    readonly code: number = code;
    constructor() {
      super(message);
      this.name = name;
    }
  };
  const ctor = (): ErrorWithCode => new cls() as ErrorWithCode;
  codeToCtor.set(code, ctor);
  nameToCtor.set(name, ctor);
  return cls as unknown as new () => ErrorWithCode;
}

export const InvalidMarketParametersError = register(0, 'InvalidMarketParameters', 'Invalid market parameters');
export const InvalidDepositAccountsError = register(1, 'InvalidDepositAccounts', 'Invalid deposit accounts');
export const InvalidWithdrawAccountsError = register(2, 'InvalidWithdrawAccounts', 'Invalid withdraw accounts');
export const InvalidCancelError = register(3, 'InvalidCancel', 'Invalid cancel');
export const InvalidFreeListError = register(4, 'InvalidFreeList', 'Internal free list corruption');
export const AlreadyClaimedSeatError = register(5, 'AlreadyClaimedSeat', 'Cannot claim a second seat for the same trader');
export const PostOnlyWouldCrossError = register(6, 'PostOnlyWouldCross', 'PostOnly order would cross the book');
export const OrderAlreadyExpiredError = register(7, 'OrderAlreadyExpired', 'Order is already expired');
export const WrongIndexHintParamsError = register(8, 'WrongIndexHintParams', 'Index hint did not match the actual order');
export const OverflowError = register(9, 'Overflow', 'Numeric overflow');
export const IncorrectAccountError = register(10, 'IncorrectAccount', 'Account key did not match expected');
export const InvalidMintError = register(11, 'InvalidMint', 'Mint not allowed for this market');
export const NoSeatClaimedError = register(12, 'NoSeatClaimed', 'Trader has no claimed seat in this market');
export const SeatHasOpenObligationsError = register(13, 'SeatHasOpenObligations', 'Seat still has open obligations');
export const InsufficientWithdrawableBalanceError = register(14, 'InsufficientWithdrawableBalance', 'Withdraw exceeds withdrawable balance');
export const RateBelowFloorError = register(15, 'RateBelowFloor', 'Cross fails the protocol-fee floor gate');
export const TermNotCompatibleError = register(16, 'TermNotCompatible', 'Bid term exceeds ask\'s max lock-up');
export const CollateralInsufficientError = register(17, 'CollateralInsufficient', 'Collateral too low for the requested principal');
export const Token2022UnsupportedExtensionError = register(18, 'Token2022UnsupportedExtension', 'Token-2022 mint has an unsupported extension');
export const OrderNotFoundError = register(19, 'OrderNotFound', 'Order does not exist');
export const OrderNotOwnedBySignerError = register(20, 'OrderNotOwnedBySigner', 'Order is not owned by the signer');
export const InvalidArgumentError = register(21, 'InvalidArgument', 'Generic invalid argument');
export const MatchedLoanNotFoundError = register(22, 'MatchedLoanNotFound', 'MatchedLoan not found in market');
export const SelfMatchForbiddenError = register(23, 'SelfMatchForbidden', 'Self-match forbidden: taker and maker share a seat');
export const CollateralBelowMatchLTVError = register(24, 'CollateralBelowMatchLTV', 'Matched collateral below required LTV at current oracle prices');
export const SecondaryNotCurrentLenderError = register(25, 'SecondaryNotCurrentLender', "SecondaryLoanSale: signer's seat is not the loan's current lender");
export const SecondaryDuplicateError = register(26, 'SecondaryDuplicate', 'SecondaryLoanSale: a resting secondary bid already exists for this loan');
export const SecondaryOrderTypeInvalidError = register(27, 'SecondaryOrderTypeInvalid', 'SecondaryLoanSale: only Limit order_type is permitted for secondary bids');
export const SecondaryLoanWrongTypeError = register(28, 'SecondaryLoanWrongType', 'SecondaryLoanSale: target loan must be Fixed (P2Pool loans are not resellable)');
export const SecondaryLoanSettledError = register(29, 'SecondaryLoanSettled', 'SecondaryLoanSale: target loan has no outstanding debt');
export const SecondarySplitLtvFailedError = register(30, 'SecondarySplitLtvFailed', 'SecondaryLoanSale split: post-split LTV check failed on at least one sub-loan');
export const SecondaryFieldImmutableError = register(31, 'SecondaryFieldImmutable', 'SecondaryLoanSale: rate / term / principal / collateral are loan-snapshots and cannot be updated');
export const SecondaryLoanMaturedError = register(32, 'SecondaryLoanMatured', 'SecondaryLoanSale: target loan has matured; reject at placement');
export const VaultWrongMintError = register(33, 'VaultWrongMint', 'vault: passed mint does not match GlobalVaultFixed.mint');
export const VaultProfileNotFoundError = register(34, 'VaultProfileNotFound', 'vault: profile_id not found in vault.risk_profiles tree');
export const VaultProfileAllowedMarketsExceededError = register(35, 'VaultProfileAllowedMarketsExceeded', 'create_risk_profile: allowed_market_max exceeds RISK_PROFILE_ALLOWED_MARKETS_CAP (8)');
export const VaultOrderTermExceedsProfileMaxError = register(36, 'VaultOrderTermExceedsProfileMax', 'vault order: term_seconds exceeds RiskProfile.max_term_seconds');
export const VaultCuratorRequiredError = register(37, 'VaultCuratorRequired', 'vault: signer is not RiskProfile.curator');
export const VaultAdminRequiredError = register(38, 'VaultAdminRequired', 'vault: signer is not GlobalVaultFixed.global_vault_admin');
export const VaultProfileSeatExistsError = register(39, 'VaultProfileSeatExists', 'claim_seat_for_risk_profile: a vault-owned ClaimedSeat already exists for (vault, profile_id) in this market');
export const VaultProfileOrderExistsError = register(40, 'VaultProfileOrderExists', 'place_order_for_risk_profile: a RiskProfileOrderRef already exists for (market, profile_id)');
export const VaultInsufficientIdleAtomsError = register(41, 'VaultInsufficientIdleAtoms', 'global_vault_withdraw: idle_principal_atoms < requested atoms (deployed liquidity cannot be withdrawn until repaid)');
export const VaultProfileNotEmptyError = register(42, 'VaultProfileNotEmpty', 'vault: profile has nonzero seats / orders / loans / shares; cannot remove');
export const VaultMarketExposureCapExceededError = register(43, 'VaultMarketExposureCapExceeded', "vault order: filled atoms would exceed market-side seat's max_exposure_atoms (per-market cap)");
export const VaultProfileIdExistsError = register(44, 'VaultProfileIdExists', 'create_risk_profile: profile_id already exists in vault');
export const VaultProfileLtvOutOfRangeError = register(45, 'VaultProfileLtvOutOfRange', 'create_risk_profile: max_ltv_bps must be between 1 and 9_999');
export const VaultProfileTermInvalidError = register(46, 'VaultProfileTermInvalid', 'create_risk_profile: max_term_seconds must be > 0');
export const LoanNotMaturedError = register(47, 'LoanNotMatured', 'settle_matured_loan: now <= matures_at_unix + grace_period_seconds (loan not yet matured)');
export const LoanStillSolventError = register(48, 'LoanStillSolvent', 'liquidate_loan: current LTV is below maintenance_ltv (loan still solvent)');
export const LiquidatorPaymentInsufficientError = register(49, 'LiquidatorPaymentInsufficient', "liquidation: liquidator's debt_token has insufficient atoms to cover outstanding");
export const LiquidationCollateralUnderflowError = register(50, 'LiquidationCollateralUnderflow', 'liquidation: collateral_atoms < required debt-equivalent + bonus');
export const MarketAdminRequiredError = register(51, 'MarketAdminRequired', 'market admin signer required (signer != MarketFixed.admin)');
export const InvalidFeeConfigError = register(52, 'InvalidFeeConfig', 'invalid fee config: bps field exceeds 10_000 or other constraint');
export const PendingAdminMismatchError = register(53, 'PendingAdminMismatch', 'admin transfer accept: signer != pending_admin');
export const MarketPausedError = register(54, 'MarketPaused', 'market is paused — state-mutating ix rejected');
export const GlobalPausedError = register(55, 'GlobalPaused', 'global pause active — state-mutating ix rejected');
export const ProtocolAdminRequiredError = register(56, 'ProtocolAdminRequired', 'protocol admin signer required (signer != GlobalConfig.protocol_admin)');
export const BorrowerLtvExceededError = register(57, 'BorrowerLtvExceeded', "borrower_ltv: actual loan LTV at oracle prices exceeds borrower's declared cap");
export const BorrowerLtvOverInitError = register(58, 'BorrowerLtvOverInit', "borrower_ltv: declared value exceeds marginfi's init LTV ceiling");

// ─── AdapterError variants (100-106) ───
// Lives in `programs/ydelta/src/protocol/mod.rs::AdapterError`; emitted by the
// marginfi/oracle adapter when CPI side-effects diverge or oracle config is
// unsupported. Numbered above 100 to leave room for protocol-error growth.
export const UnexpectedAtomDeltaError = register(100, 'UnexpectedAtomDelta', 'Post-CPI atom delta deviated from expected by more than 1 atom');
export const InvalidIntegrationAccountError = register(101, 'InvalidIntegrationAccount', 'Integration account header had the wrong discriminator');
export const OracleStaleError = register(102, 'OracleStale', 'Oracle price was older than the configured staleness threshold');
export const OracleSetupUnsupportedError = register(103, 'OracleSetupUnsupported', "Bank's oracle_setup is unsupported (yDelta supports PythPush, SwitchboardPull, and StakedWithPythPush only)");
export const OracleMaxConfidenceExceededError = register(104, 'OracleMaxConfidenceExceeded', "Confidence interval exceeded the bank's oracle_max_confidence");
export const OracleAccountMismatchError = register(105, 'OracleAccountMismatch', "Caller supplied oracle account doesn't match the bank's configured oracle pubkey");
export const OracleNonPositiveError = register(106, 'OracleNonPositive', 'Oracle reported a non-positive price');

export function errorFromCode(code: number): MaybeErrorWithCode {
  const ctor = codeToCtor.get(code);
  return ctor ? ctor() : null;
}

export function errorFromName(name: string): MaybeErrorWithCode {
  const ctor = nameToCtor.get(name);
  return ctor ? ctor() : null;
}

/** Match an error code emitted by `solana_program::program_error::ProgramError::Custom(u32)`.
 *  The on-chain program returns `Custom(<error_code>)`; web3.js surfaces this in tx logs as
 *  `custom program error: 0x...`. */
export function errorFromCustomCode(customCode: number): MaybeErrorWithCode {
  return errorFromCode(customCode);
}
