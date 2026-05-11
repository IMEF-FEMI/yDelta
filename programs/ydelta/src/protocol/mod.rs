//! Lending protocol adapters used by yDelta core.

pub mod marginfi;
pub mod oracles;

use solana_program::{account_info::AccountInfo, program_error::ProgramError};

/// Identifies which money-market integration an adapter wraps.
/// Stored on `MarketFixed` so the right adapter is dispatched per
/// market.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProtocolId {
    Marginfi = 0,
    // Reserved: Kamino = 1, Drift = 2, Solend = 3, …
}

/// Errors that can bubble out of an adapter call. Variants here flag
/// adapter-specific failure modes the matching engine and accrue paths need
/// to distinguish from underlying-protocol errors (which propagate as-is
/// via `?`). Numbering starts at 100 to leave the 0..99 range to
/// `YdeltaError` — same convention marginfi follows for its mock crates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AdapterError {
    /// Post-CPI atom delta deviated from expected by more than 1 atom — the
    /// adapter's safety net for share-price truncation drift. Mirrors
    /// marginfi's own `assert_within_one_token` pattern.
    UnexpectedAtomDelta = 100,
    /// Integration account header had the wrong discriminator.
    InvalidIntegrationAccount = 101,
    /// Oracle price was older than the configured staleness threshold.
    OracleStale = 102,
    /// Bank's `oracle_setup` is one we don't support (currently only
    /// Pyth-push and Switchboard-pull are supported).
    OracleSetupUnsupported = 103,
    /// Confidence interval exceeded the bank's `oracle_max_confidence`.
    OracleMaxConfidenceExceeded = 104,
    /// Caller supplied an `accounts[1]` that doesn't match the bank's
    /// configured oracle pubkey.
    OracleAccountMismatch = 105,
    /// Oracle reported a non-positive price, which would corrupt fp48
    /// math downstream.
    OracleNonPositive = 106,
}

impl From<AdapterError> for ProgramError {
    fn from(e: AdapterError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

/// Trait every money-market adapter implements. CPI methods accept raw
/// `&[AccountInfo]` slices because each underlying protocol has a different
/// account list shape; the adapter file knows the order and validates
/// accordingly.
///
/// **Slice layout convention** (per-method, mirrors Anchor's
/// `ctx.remaining_accounts`):
/// - `accounts[..N]` are the explicit accounts the protocol's IDL declares,
///   in IDL order. The adapter validates these.
/// - `accounts[N..]` are protocol-specific extras the underlying program
///   reads variadically — for marginfi, the `(bank, oracle)` pairs every
///   `withdraw`/`borrow` needs for its post-op solvency check (see
///   `marginfi_mocks::cpi` doc comments). The adapter forwards these
///   verbatim without inspection; callers are responsible for ordering.
///
/// Methods take `&self` because the adapter is a zero-sized type — all
/// integration-specific data lives in the accounts the caller passes in.
/// This matches marginfi's `Bank` pattern (state on the bank account, no
/// per-instance state on the trait impl).
pub trait LendingProtocol {
    fn id(&self) -> ProtocolId;

    // ─────────────────── CPI methods ───────────────────

    /// Deposit `amount_atoms` from `signer_token_account` into the
    /// integration pool. Returns the number of `u128` (fp48) shares minted.
    fn deposit<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u128, ProgramError>;

    /// Withdraw `shares` from the integration pool into
    /// `destination_token_account`. Returns atoms received.
    fn withdraw<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        shares: u128,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u64, ProgramError>;

    /// Open an integration borrow of `amount_atoms` against the
    /// marginfi-account referenced in `accounts`. Returns the number of
    /// liability shares opened.
    fn borrow<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u128, ProgramError>;

    /// Repay `shares` of an open borrow. Returns atoms paid.
    fn repay<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        shares: u128,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u64, ProgramError>;

    /// Repay an exact `amount_atoms` against an open borrow. Returns the
    /// liability shares burned. Mirrors `deposit`'s atom-in / shares-out
    /// shape — useful when the caller knows the atom amount (e.g. a
    /// borrower repaying `loan.outstanding_debt_atoms`) rather than the
    /// share count.
    fn repay_atoms<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u128, ProgramError>;

    // ─────────────────── Read-only queries ───────────────────

    /// Convert integration shares to atoms at the current share price. Reads
    /// the bank account; no CPI.
    fn shares_to_amount<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        shares: u128,
    ) -> Result<u64, ProgramError>;

    /// Convert atoms to **asset-side** integration shares at the bank's
    /// current `asset_share_value`. Reads the bank account; no CPI.
    /// Use this for lender-side bookkeeping (deposit, claim_repayment,
    /// settle_matured_loan, liquidate_loan, do_vault_settle).
    fn amount_to_asset_shares<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
    ) -> Result<u128, ProgramError>;

    /// Convert atoms to **liability-side** integration shares at the
    /// bank's current `liability_share_value`. Reads the bank account;
    /// no CPI. Use this for borrower-side bookkeeping
    /// (P2Pool fallback's marginfi.borrow share patch, future
    /// ConvertP2PoolToFixed liability sizing).
    fn amount_to_liability_shares<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
    ) -> Result<u128, ProgramError>;

    /// Current oracle quote-per-base price as `u128` (fp48).
    fn oracle_price<'info>(&self, accounts: &[AccountInfo<'info>]) -> Result<u128, ProgramError>;

    /// `(asset_weight, liability_weight)` at initialisation tier.
    fn init_weight<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
    ) -> Result<(u128, u128), ProgramError>;

    /// `(asset_weight, liability_weight)` at maintenance tier.
    fn maint_weight<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
    ) -> Result<(u128, u128), ProgramError>;
}
