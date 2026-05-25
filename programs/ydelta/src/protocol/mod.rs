//! External-program adapters. Wraps marginfi v0.1.8 CPIs
//! ([`marginfi`]), the off-chain accrue-prediction math ([`marginfi_rate_calc`]),
//! and Pyth-push + Switchboard-pull oracle decoders ([`oracles`]) behind
//! the generic [`LendingProtocol`] trait so the processor layer stays
//! free of protocol-specific account-layout knowledge.

pub mod marginfi;
pub mod marginfi_rate_calc;
pub mod oracles;

use solana_program::{account_info::AccountInfo, program_error::ProgramError};

/// Tag for the lending protocol an adapter wraps. Currently only
/// marginfi v0.1.8 is implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProtocolId {
    /// marginfi v0.1.8 adapter (see [`marginfi::MarginfiV18Adapter`]).
    Marginfi = 0,
}

/// Adapter-side errors. Lowered to `ProgramError::Custom(u32)` via
/// `From<AdapterError>`; discriminants start at 100 to stay disjoint
/// from [`crate::program::YdeltaError`]. Renumbering is a breaking
/// change for off-chain decoders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AdapterError {
    /// Post-CPI atom delta deviated from the predicted value by more
    /// than 1 atom (refuses silent share/atom drift).
    UnexpectedAtomDelta = 100,

    /// Account passed to an adapter is not owned by the expected
    /// program or fails the protocol-specific layout check.
    InvalidIntegrationAccount = 101,

    /// Oracle publish_time is outside the `[now - max_age, now + skew]`
    /// freshness window.
    OracleStale = 102,

    /// Oracle setup (Pyth Legacy, Switchboard V2, Kamino-wrapped, etc)
    /// is recognized but not supported by this program.
    OracleSetupUnsupported = 103,

    /// Inflated confidence interval exceeds the bank's `oracle_max_confidence`.
    OracleMaxConfidenceExceeded = 104,

    /// Oracle account key does not match `bank.config.oracle_keys[i]`
    /// or its owner is not the expected oracle program.
    OracleAccountMismatch = 105,

    /// Oracle price or derived value was zero / negative — refuses to
    /// proceed with a degenerate reading.
    OracleNonPositive = 106,
}

impl From<AdapterError> for ProgramError {
    fn from(e: AdapterError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

/// Common surface for external lending protocols. Adapters take a
/// `&[AccountInfo]` slice positioned at the protocol-specific account
/// layout, run the CPI under `signer_seeds`, and return the realized
/// share / atom delta read back from the post-CPI account state.
pub trait LendingProtocol {
    /// Returns the [`ProtocolId`] tag identifying the underlying protocol.
    fn id(&self) -> ProtocolId;

    /// Deposit `amount_atoms` into the protocol pool. Returns the
    /// fp48 asset shares actually minted (post-CPI delta).
    fn deposit<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u128, ProgramError>;

    /// Burn `shares` fp48 asset shares. Returns `(atoms_received,
    /// shares_actually_burned)` read back from the destination ATA and
    /// the marginfi account.
    fn withdraw<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        shares: u128,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<(u64, u128), ProgramError>;

    /// Open a borrow for `amount_atoms`. Returns the fp48 liability
    /// shares minted (post-CPI delta).
    fn borrow<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u128, ProgramError>;

    /// Repay `shares` fp48 liability shares (atoms computed at the
    /// current `liability_share_value`, ceiled). Returns atoms paid in.
    fn repay<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        shares: u128,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u64, ProgramError>;

    /// Repay exactly `amount_atoms` against the borrow. Returns the
    /// fp48 liability shares actually burned (post-CPI delta).
    fn repay_atoms<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u128, ProgramError>;

    /// Off-chain: convert fp48 asset `shares` to atoms at the current
    /// `asset_share_value`, floored.
    fn shares_to_amount<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        shares: u128,
    ) -> Result<u64, ProgramError>;

    /// Off-chain: convert `amount_atoms` to fp48 asset shares at the
    /// current `asset_share_value`, floored.
    fn amount_to_asset_shares<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
    ) -> Result<u128, ProgramError>;

    /// Off-chain: convert `amount_atoms` to fp48 liability shares at
    /// the current `liability_share_value`, floored.
    fn amount_to_liability_shares<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
    ) -> Result<u128, ProgramError>;

    /// Read the fp48 oracle price for the bank account positioned at
    /// `accounts[0]`. Includes staleness + confidence gating.
    fn oracle_price<'info>(&self, accounts: &[AccountInfo<'info>]) -> Result<u128, ProgramError>;

    /// Returns the bank's fp48 `(asset_weight_init, liability_weight_init)`.
    fn init_weight<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
    ) -> Result<(u128, u128), ProgramError>;

    /// Returns the bank's fp48 `(asset_weight_maint, liability_weight_maint)`.
    fn maint_weight<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
    ) -> Result<(u128, u128), ProgramError>;
}
