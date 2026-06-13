//! Program error enum. Every variant lowers to `ProgramError::Custom(u32)`
//! via the `From<YdeltaError>` impl below; the `u32` is the explicit
//! `repr(u32)` discriminant on the variant. Discriminants are stable —
//! the SDK and indexer decode them by value, so renumbering an existing
//! variant is a breaking change. New variants append at the end.
//!
//! The `#[error("...")]` attribute on each variant is the human-readable
//! message surfaced by the `thiserror` `Display` impl (and the one shown
//! in tx logs via `msg!`).

use num_enum::TryFromPrimitive;
use solana_program::program_error::ProgramError;
use thiserror::Error;

/// Custom-error namespace for the ydelta program. All variants serialize
/// as `ProgramError::Custom(self as u32)`; the discriminant is the
/// public contract. Gaps in the numbering (e.g. 27, 31, 35) are retired
/// variants — never re-use a retired discriminant.
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

    #[error("vault: passed mint does not match GlobalVaultFixed.mint")]
    VaultWrongMint = 25,
    #[error("vault: sub_vault_id not found in vault.sub_vaults tree")]
    SubVaultNotFound = 26,

    #[error("vault order: term_seconds exceeds SubVault.max_term_seconds")]
    VaultOrderTermExceedsProfileMax = 28,
    #[error("vault: signer is not SubVault.curator")]
    VaultCuratorRequired = 29,
    #[error("vault: signer is not GlobalVaultFixed.global_vault_admin")]
    VaultAdminRequired = 30,

    #[error("place_order_for_sub_vault: a SubVaultOrderRef already exists for (market, sub_vault_id)")]
    SubVaultOrderExists = 32,
    #[error("global_vault_withdraw: idle_principal_atoms < requested atoms (deployed liquidity cannot be withdrawn until repaid)")]
    VaultInsufficientIdleAtoms = 33,
    #[error("vault: profile has nonzero seats / orders / loans / shares; cannot remove")]
    SubVaultNotEmpty = 34,

    #[error("create_sub_vault: sub_vault_id already exists in vault")]
    SubVaultIdExists = 36,
    #[error("create_sub_vault: max_ltv_bps must be between 1 and 9_999")]
    SubVaultLtvOutOfRange = 37,
    #[error("create_sub_vault: max_term_seconds must be > 0")]
    SubVaultTermInvalid = 38,

    #[error(
        "settle_matured_loan: now <= matures_at_unix + grace_period_seconds (loan not yet matured)"
    )]
    LoanNotMatured = 39,
    #[error("liquidate_loan: current LTV is below maintenance_ltv (loan still solvent)")]
    LoanStillSolvent = 40,
    #[error("liquidation: liquidator's debt_token has insufficient atoms to cover outstanding")]
    LiquidatorPaymentInsufficient = 41,
    #[error("liquidation: collateral_atoms < required debt-equivalent + bonus (bad debt logged via BadDebtLog; insurance fund coverage is post-mainnet)")]
    LiquidationCollateralUnderflow = 42,
    #[error("market admin signer required (signer != MarketFixed.admin)")]
    MarketAdminRequired = 43,
    #[error("invalid fee config: bps field exceeds 10_000 or other constraint")]
    InvalidFeeConfig = 44,
    #[error("admin transfer accept: signer != pending_admin")]
    PendingAdminMismatch = 45,
    #[error("market is paused — state-mutating ix rejected")]
    MarketPaused = 46,
    #[error("global pause active — state-mutating ix rejected")]
    GlobalPaused = 47,
    #[error("protocol admin signer required (signer != GlobalConfig.protocol_admin)")]
    ProtocolAdminRequired = 48,
    #[error("degenerate oracle reading (zero price or zero weight) — liquidation gate cannot prove a breach")]
    OracleDegenerate = 49,
    #[error("vault is paused — state-mutating ix rejected")]
    VaultPaused = 50,

    #[error("math: division by zero")]
    MathDivisionByZero = 51,
    #[error("math: arithmetic overflow in fixed-point or wide-multiply helper")]
    MathOverflow = 52,

    #[error("loan close: seat's encumbered collateral is less than the loan's recorded \
             collateral — state corruption, refusing silent collateral drop")]
    InsufficientEncumberedCollateral = 53,

    #[error("sub-vault is sunset: new deposits / new orders / order updates / \
             matches are rejected; only withdrawals, fee claims, and cancellations \
             are allowed during wind-down")]
    SubVaultSunset = 54,

    #[error("sub-vault is not sunset: this admin operation requires the profile \
             to be in sunset state first (call SunsetSubVault)")]
    SubVaultNotSunset = 55,

    #[error("P2Pool fallback: residual collateral is below marginfi's init-weight \
             requirement — the marginfi borrow would fail its health check \
             (marginfi weights gate ONLY the fallback; use Rest/Drop residual modes \
             or post more collateral)")]
    FallbackLtvInsufficient = 56,
}

impl From<YdeltaError> for ProgramError {
    fn from(e: YdeltaError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

/// Guard-clause macro. If `$test` evaluates to `false`, logs a
/// `[file:line] message` line (via `msg!` on-chain, `println!` off-chain)
/// and returns `Err($err.into())` from the caller. Returns `Ok(())` on
/// success, so it composes with `?`.
///
/// `$err` may be any value with `Into<ProgramError>` — typically a
/// [`YdeltaError`] variant, but `ProgramError::*` works too.
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
