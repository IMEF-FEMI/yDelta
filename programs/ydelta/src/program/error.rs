use num_enum::TryFromPrimitive;
use solana_program::program_error::ProgramError;
use thiserror::Error;

/// Error taxonomy for yDelta. Discriminants are contiguous; new
/// variants append at the next number.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u32)]
pub enum YdeltaError {
    #[error("Invalid market parameters")]
    InvalidMarketParameters = 0,
    #[error("Invalid deposit accounts")]
    InvalidDepositAccounts = 1,
    #[error("Invalid withdraw accounts")]
    InvalidWithdrawAccounts = 2,
    #[error("Invalid cancel")]
    InvalidCancel = 3,
    #[error("Internal free list corruption")]
    InvalidFreeList = 4,
    #[error("Cannot claim a second seat for the same trader")]
    AlreadyClaimedSeat = 5,
    #[error("PostOnly order would cross the book")]
    PostOnlyWouldCross = 6,
    #[error("Order is already expired")]
    OrderAlreadyExpired = 7,
    #[error("Index hint did not match the actual order")]
    WrongIndexHintParams = 8,
    #[error("Numeric overflow")]
    Overflow = 9,
    #[error("Account key did not match expected")]
    IncorrectAccount = 10,
    #[error("Mint not allowed for this market")]
    InvalidMint = 11,
    #[error("Trader has no claimed seat in this market")]
    NoSeatClaimed = 12,
    #[error("Seat still has open obligations")]
    SeatHasOpenObligations = 13,
    #[error("Withdraw exceeds withdrawable balance")]
    InsufficientWithdrawableBalance = 14,
    #[error("Cross fails the protocol-fee floor gate")]
    RateBelowFloor = 15,
    #[error("Bid term exceeds ask's max lock-up")]
    TermNotCompatible = 16,
    #[error("Collateral too low for the requested principal")]
    CollateralInsufficient = 17,
    #[error("Token-2022 mint has an unsupported extension")]
    Token2022UnsupportedExtension = 18,
    #[error("Order does not exist")]
    OrderNotFound = 19,
    #[error("Order is not owned by the signer")]
    OrderNotOwnedBySigner = 20,
    #[error("Generic invalid argument")]
    InvalidArgument = 21,
    #[error("MatchedLoan not found in market")]
    MatchedLoanNotFound = 22,
    #[error("Self-match forbidden: taker and maker share a seat")]
    SelfMatchForbidden = 23,
    #[error("Matched collateral below required LTV at current oracle prices")]
    CollateralBelowMatchLTV = 24,
    // ─── Secondary loan sale (lender OB exit) ───
    #[error("SecondaryLoanSale: signer's seat is not the loan's current lender")]
    SecondaryNotCurrentLender = 25,
    #[error("SecondaryLoanSale: a resting secondary bid already exists for this loan")]
    SecondaryDuplicate = 26,
    #[error("SecondaryLoanSale: only Limit order_type is permitted for secondary bids")]
    SecondaryOrderTypeInvalid = 27,
    #[error("SecondaryLoanSale: target loan must be Fixed (P2Pool loans are not resellable)")]
    SecondaryLoanWrongType = 28,
    #[error("SecondaryLoanSale: target loan has no outstanding debt")]
    SecondaryLoanSettled = 29,
    #[error("SecondaryLoanSale split: post-split LTV check failed on at least one sub-loan")]
    SecondarySplitLtvFailed = 30,
    #[error("SecondaryLoanSale: rate / term / principal / collateral are loan-snapshots and cannot be updated")]
    SecondaryFieldImmutable = 31,
    #[error("SecondaryLoanSale: target loan has matured; reject at placement")]
    SecondaryLoanMatured = 32,
    // ─── GlobalVault ───
    #[error("vault: passed mint does not match GlobalVaultFixed.mint")]
    VaultWrongMint = 33,
    #[error("vault: profile_id not found in vault.risk_profiles tree")]
    VaultProfileNotFound = 34,
    #[error(
        "create_risk_profile: allowed_market_max exceeds RISK_PROFILE_ALLOWED_MARKETS_CAP (8)"
    )]
    VaultProfileAllowedMarketsExceeded = 35,
    #[error("vault order: term_seconds exceeds RiskProfile.max_term_seconds")]
    VaultOrderTermExceedsProfileMax = 36,
    #[error("vault: signer is not RiskProfile.curator")]
    VaultCuratorRequired = 37,
    #[error("vault: signer is not GlobalVaultFixed.global_vault_admin")]
    VaultAdminRequired = 38,
    #[error("claim_seat_for_risk_profile: a vault-owned ClaimedSeat already exists for (vault, profile_id) in this market")]
    VaultProfileSeatExists = 39,
    #[error("place_order_for_risk_profile: a RiskProfileOrderRef already exists for (market, profile_id)")]
    VaultProfileOrderExists = 40,
    #[error("global_vault_withdraw: idle_principal_atoms < requested atoms (deployed liquidity cannot be withdrawn until repaid)")]
    VaultInsufficientIdleAtoms = 41,
    #[error("vault: profile has nonzero seats / orders / loans / shares; cannot remove")]
    VaultProfileNotEmpty = 42,
    #[error("vault order: filled atoms would exceed market-side seat's max_exposure_atoms (per-market cap)")]
    VaultMarketExposureCapExceeded = 43,
    #[error("create_risk_profile: profile_id already exists in vault")]
    VaultProfileIdExists = 44,
    #[error("create_risk_profile: max_ltv_bps must be between 1 and 9_999")]
    VaultProfileLtvOutOfRange = 45,
    #[error("create_risk_profile: max_term_seconds must be > 0")]
    VaultProfileTermInvalid = 46,
    // ─── Liquidation ───
    #[error(
        "settle_matured_loan: now <= matures_at_unix + grace_period_seconds (loan not yet matured)"
    )]
    LoanNotMatured = 47,
    #[error("liquidate_loan: current LTV is below maintenance_ltv (loan still solvent)")]
    LoanStillSolvent = 48,
    #[error("liquidation: liquidator's debt_token has insufficient atoms to cover outstanding")]
    LiquidatorPaymentInsufficient = 49,
    #[error("liquidation: collateral_atoms < required debt-equivalent + bonus (bad debt logged via BadDebtLog; insurance fund coverage is post-mainnet)")]
    LiquidationCollateralUnderflow = 50,
    #[error("market admin signer required (signer != MarketFixed.admin)")]
    MarketAdminRequired = 51,
    #[error("invalid fee config: bps field exceeds 10_000 or other constraint")]
    InvalidFeeConfig = 52,
    #[error("admin transfer accept: signer != pending_admin")]
    PendingAdminMismatch = 53,
    #[error("market is paused — state-mutating ix rejected")]
    MarketPaused = 54,
    #[error("global pause active — state-mutating ix rejected")]
    GlobalPaused = 55,
    #[error("protocol admin signer required (signer != GlobalConfig.protocol_admin)")]
    ProtocolAdminRequired = 56,
    #[error("borrower_ltv: actual loan LTV at oracle prices exceeds borrower's declared cap")]
    BorrowerLtvExceeded = 57,
    #[error("borrower_ltv: declared value exceeds marginfi's init LTV ceiling")]
    BorrowerLtvOverInit = 58,
}

impl From<YdeltaError> for ProgramError {
    fn from(e: YdeltaError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

#[macro_export]
macro_rules! require {
    ($test:expr, $err:expr, $($arg:tt)*) => {
        if $test {
            Ok::<(), solana_program::program_error::ProgramError>(())
        } else {
            #[cfg(target_os = "solana")]
            solana_program::msg!("[{}:{}] {}", std::file!(), std::line!(), std::format_args!($($arg)*));
            #[cfg(not(target_os = "solana"))]
            std::println!("[{}:{}] {}", std::file!(), std::line!(), std::format_args!($($arg)*));
            Err(($err).into())
        }
    };
}
