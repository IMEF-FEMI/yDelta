use bytemuck::{Pod, Zeroable};
use hypertree::DataIndex;
use shank::ShankAccount;
use solana_program::{program_error::ProgramError, pubkey::Pubkey};

use crate::utils::get_discriminant;

/// Serialise an event onto the stack and emit it via `sol_log_data`. This
/// is cheaper than a self-CPI; trades a small CU cost for one fewer signer
/// account in every ix that emits.
#[inline(never)]
pub fn emit_stack<T: bytemuck::Pod + Discriminant>(e: T) -> Result<(), ProgramError> {
    // Compile-time guard: an oversized log struct would overrun the
    // fixed stack buffer below. Fail the build, not at runtime.
    const {
        assert!(
            std::mem::size_of::<T>() + 8 <= 3000,
            "log struct exceeds emit_stack buffer",
        )
    };
    let mut buffer: [u8; 3000] = [0u8; 3000];
    buffer[..8].copy_from_slice(&T::discriminant());
    *bytemuck::from_bytes_mut::<T>(&mut buffer[8..8 + std::mem::size_of::<T>()]) = e;

    solana_program::log::sol_log_data(&[&buffer[..(std::mem::size_of::<T>() + 8)]]);
    Ok(())
}

pub trait Discriminant {
    fn discriminant() -> [u8; 8];
}

macro_rules! impl_discriminant {
    ($t:ident) => {
        impl Discriminant for $t {
            fn discriminant() -> [u8; 8] {
                // `stringify!` yields a stable source-level string,
                // unlike `std::any::type_name`.
                u64::to_le_bytes(get_discriminant(stringify!($t)))
            }
        }
    };
}

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct CreateMarketLog {
    pub market: Pubkey,
    pub creator: Pubkey,
    pub debt_mint: Pubkey,
    pub collateral_mint: Pubkey,
}
impl_discriminant!(CreateMarketLog);

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct ClaimSeatLog {
    pub market: Pubkey,
    pub trader: Pubkey,
}
impl_discriminant!(ClaimSeatLog);

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct DepositLog {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub mint: Pubkey,
    pub amount_atoms: u64,
}
impl_discriminant!(DepositLog);

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct WithdrawLog {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub mint: Pubkey,
    pub amount_atoms: u64,
}
impl_discriminant!(WithdrawLog);

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct OrderPlacedLog {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub trader_seat_index: DataIndex,
    pub side: u8,
    /// Reserved byte. Always 0.
    pub _reserved_kind: u8,
    pub order_type: u8,
    pub _padding1: u8,
    pub rate_bps: u16,
    pub _padding2: u16,
    pub term_seconds: u32,
    pub principal_atoms: u64,
    pub collateral_atoms: u64,
    pub sequence: u64,
    pub last_valid_unix_ts: i64,
}
impl_discriminant!(OrderPlacedLog);

/// Emitted per match. `loan_pda` is `Pubkey::default()` until the
/// cranker promotes the queue node into a `LoanFixed` PDA. `flags`
/// carries the queue node's `MATCHED_LOAN_FLAG_*` bits (`VAULT_LENDER`,
/// `VAULT_PRESETTLED`) so off-chain indexers can classify the match
/// without re-reading the queue node.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct MatchedLoanCreatedLog {
    pub market: Pubkey,
    pub loan_pda: Pubkey,
    pub sequence: u64,
    pub lender_seat_index: DataIndex,
    pub borrower_seat_index: DataIndex,
    pub principal_atoms: u64,
    pub collateral_atoms: u64,
    pub borrower_rate_bps: u16,
    pub lender_rate_bps: u16,
    pub term_seconds: u32,
    pub matched_at_unix: i64,
    pub loan_type: u8,
    pub flags: u8,
    pub _padding: [u8; 6],
}
impl_discriminant!(MatchedLoanCreatedLog);

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct OrderExpiredLog {
    pub market: Pubkey,
    pub owner_seat_index: DataIndex,
    pub side: u8,
    pub _padding: [u8; 3],
    pub sequence: u64,
}
impl_discriminant!(OrderExpiredLog);

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct OrderRestedLog {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub sequence: u64,
    pub principal_remaining_atoms: u64,
    pub last_valid_unix_ts: i64,
    pub rate_bps: u16,
    pub side: u8,
    pub _padding: [u8; 1],
    pub term_seconds: u32,
}
impl_discriminant!(OrderRestedLog);

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct OrderFilledIocLog {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub sequence: u64,
    pub principal_dropped_atoms: u64,
    pub side: u8,
    pub _padding: [u8; 7],
}
impl_discriminant!(OrderFilledIocLog);

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct CancelOrderLog {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub sequence: u64,
}
impl_discriminant!(CancelOrderLog);

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct UpdateOrderLog {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub old_sequence: u64,
    pub new_sequence: u64,
}
impl_discriminant!(UpdateOrderLog);

/// Emitted by `process_matched_loan` when promoting a `MatchedLoan`
/// node into a `LoanFixed` PDA. `principal_atoms` is the gross matched
/// principal; `net_principal_atoms` is what landed in the borrower's
/// seat after the origination fee was deducted; `credited_shares` is
/// that net amount in the debt bank's fp48 share units.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct LoanPromotedLog {
    pub market: Pubkey,
    pub loan: Pubkey,
    pub sequence: u64,
    pub cranker: Pubkey,
    pub principal_atoms: u64,
    pub net_principal_atoms: u64,
    pub origination_atoms: u64,
    pub credited_shares: u128,
    pub origination_shares: u128,
}
impl_discriminant!(LoanPromotedLog);

/// Borrower-repay log. `outstanding_after` is the post-repay
/// `LoanFixed.outstanding_debt_atoms`; `full_repay = 1` iff the loan
/// transitioned to `LoanState::Repaid` in this ix.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct LoanRepaidLog {
    pub market: Pubkey,
    pub loan: Pubkey,
    pub borrower: Pubkey,
    pub repay_atoms: u64,
    pub outstanding_after: u64,
    pub full_repay: u8,
    pub _padding: [u8; 7],
}
impl_discriminant!(LoanRepaidLog);

/// Lender-claim log. `claimed_atoms` is `lender_claimable_atoms` at
/// the start of the claim; `claimed_shares` is that amount in
/// debt-bank fp48 share units credited to the lender's seat;
/// `protocol_fee_shares_swept` is the spread accumulated over the
/// loan's life that just landed on
/// `market.accumulated_protocol_fee_shares` (non-zero only on the
/// closing claim); `closed = 1` iff this claim closed the Loan PDA.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct LoanClaimedLog {
    pub market: Pubkey,
    pub loan: Pubkey,
    pub lender: Pubkey,
    pub claimed_atoms: u64,
    pub _pad0: [u8; 8],
    pub claimed_shares: u128,
    pub protocol_fee_shares_swept: u128,
    pub closed: u8,
    pub _padding: [u8; 15],
}
impl_discriminant!(LoanClaimedLog);

/// Emitted by `claim_repayment_for_risk_profile` on a fully-realised
/// risk-profile loan. `claimed_atoms` is the actual atoms drained from
/// `market.lender_integration_account` into the GlobalVault's marginfi
/// account; `principal_atoms` is the loan's original principal (used by
/// per-market exposure-cap accounting); `protocol_fee_shares_swept` is
/// the spread that just landed on `market.accumulated_protocol_fee_shares`.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct RepaymentClaimedForRiskProfileLog {
    pub market: Pubkey,
    pub loan: Pubkey,
    pub global_vault: Pubkey,
    pub risk_profile_id: u8,
    pub _pad0: [u8; 7],
    pub claimed_atoms: u64,
    pub principal_atoms: u64,
    pub _pad1: [u8; 8],
    pub protocol_fee_shares_swept: u128,
}
impl_discriminant!(RepaymentClaimedForRiskProfileLog);

// ─────────────────── `GlobalVault` ───────────────────

/// Emitted by `create_vault`. Carries the new vault PDA, the mint it
/// wraps, the admin pubkey set at creation (also the signer who paid
/// rent), and the marginfi integration_account / signer PDAs for
/// off-chain indexers.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct VaultCreatedLog {
    pub global_vault: Pubkey,
    pub mint: Pubkey,
    pub global_vault_admin: Pubkey,
    pub integration_pool: Pubkey,
    pub integration_account: Pubkey,
    pub global_vault_signer: Pubkey,
}
impl_discriminant!(VaultCreatedLog);

/// Emitted by `create_risk_profile`. Carries the vault, the new
/// profile_id, curator pubkey, and the risk policy admin set at
/// creation.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct RiskProfileCreatedLog {
    pub global_vault: Pubkey,
    pub curator: Pubkey,
    pub profile_id: u8,
    /// Reserved. Always zero.
    pub _reserved0: u8,
    pub _pad0: [u8; 2],
    pub max_ltv_bps: u16,
    pub _pad1: [u8; 2],
    pub max_term_seconds: u32,
    pub _pad2: [u8; 4],
}
impl_discriminant!(RiskProfileCreatedLog);

/// Emitted by `remove_risk_profile`. Carries the vault and the removed
/// profile_id. The id is **not** re-issued (the vault's
/// `next_profile_id` monotonic counter is unaffected by removal), so
/// any post-removal off-chain reference to this id remains a stable,
/// historical address.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct RiskProfileRemovedLog {
    pub global_vault: Pubkey,
    pub curator: Pubkey,
    pub profile_id: u8,
    pub _pad0: [u8; 7],
}
impl_discriminant!(RiskProfileRemovedLog);

/// Emitted by `global_vault_deposit`. Records atoms in, shares
/// minted, and the running profile totals after the mint.
/// `gain_atoms` reports the depositor's crystallised yield since
/// their last interaction (zero on first deposit). u128s placed
/// first for 16-byte alignment.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct GlobalVaultDepositLog {
    pub global_vault: Pubkey,            // 0..32
    pub depositor: Pubkey,               // 32..64
    pub shares_minted: u128,             // 64..80
    pub profile_total_shares: u128,      // 80..96
    pub atoms_in: u64,                   // 96..104
    pub gain_atoms: u64,                 // 104..112
    pub profile_total_assets_atoms: u64, // 112..120
    pub profile_id: u8,                  // 120..121
    pub _padding: [u8; 7],               // 121..128
}
impl_discriminant!(GlobalVaultDepositLog);

/// Emitted by `place_order_for_risk_profile`. Records the resting
/// order's market sequence number, rate, and term.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct PlaceOrderForRiskProfileLog {
    pub global_vault: Pubkey,
    pub market: Pubkey,
    pub profile_id: u8,
    pub side: u8,
    pub _pad0: [u8; 6],
    pub rate_bps: u16,
    pub _pad1: [u8; 2],
    pub term_seconds: u32,
    pub order_sequence_in_market: u64,
}
impl_discriminant!(PlaceOrderForRiskProfileLog);

/// Emitted by `cancel_order_for_risk_profile` and (with `is_replace =
/// 1`) by `update_order_for_risk_profile`'s cancel leg.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct CancelOrderForRiskProfileLog {
    pub global_vault: Pubkey,
    pub market: Pubkey,
    pub profile_id: u8,
    pub is_replace: u8,
    pub _pad0: [u8; 6],
    pub order_sequence_in_market: u64,
}
impl_discriminant!(CancelOrderForRiskProfileLog);

/// Emitted by `global_vault_withdraw`. Records shares burned, atoms
/// out, and post-withdraw profile totals.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct GlobalVaultWithdrawLog {
    pub global_vault: Pubkey,            // 0..32
    pub depositor: Pubkey,               // 32..64
    pub shares_burned: u128,             // 64..80
    pub profile_total_shares: u128,      // 80..96
    pub atoms_out: u64,                  // 96..104
    pub profile_total_assets_atoms: u64, // 104..112
    pub profile_id: u8,                  // 112..113
    pub _padding: [u8; 15],              // 113..128
}
impl_discriminant!(GlobalVaultWithdrawLog);

/// Emitted by `settle_matured_loan` and `liquidate_loan`.
/// `liquidation_kind`: 0 = matured (time-gated), 1 = LTV-breach.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct LoanLiquidatedLog {
    pub market: Pubkey,               // 0..32
    pub loan: Pubkey,                 // 32..64
    pub liquidator: Pubkey,           // 64..96
    pub debt_paid_atoms: u64,         // 96..104
    pub collateral_seized_atoms: u64, // 104..112
    pub liquidation_kind: u8,         // 112..113
    /// 0 = full repay (loan moves to Repaid), 1 = partial (loan stays Active).
    pub is_partial: u8, // 113..114
    pub _padding: [u8; 14],           // 114..128
}
impl_discriminant!(LoanLiquidatedLog);

/// Emitted by `liquidate_loan` when a loan settles underwater
/// (collateral value cannot cover debt + keeper bonus at oracle
/// prices). The lender is short by `gap_atoms` denominated in
/// collateral-mint atoms.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct BadDebtLog {
    pub market: Pubkey, // 0..32
    pub loan: Pubkey,   // 32..64
    /// `(debt_value_in_collateral_atoms + bonus_atoms) - collateral_atoms`.
    /// Strictly > 0 when this log is emitted.
    pub gap_collateral_atoms: u64, // 64..72
    pub debt_atoms_remaining: u64, // 72..80
    pub _padding: [u8; 16], // 80..96
}
impl_discriminant!(BadDebtLog);

/// Emitted by `ConvertP2PoolToFixed`. Records the closeout of a
/// P2Pool loan's marginfi.borrow liability and the in-place rewrite
/// to `LoanType::Fixed` against an existing primary Ask.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct P2PoolConvertedToFixedLog {
    pub market: Pubkey,                   // 0..32
    pub loan: Pubkey,                     // 32..64
    pub borrower: Pubkey,                 // 64..96
    pub new_lender_seat_index: DataIndex, // 96..100
    pub _pad0: [u8; 4],                   // 100..104
    pub matched_principal_atoms: u64,     // 104..112
    pub borrow_shares_burned: u128,       // 112..128
    pub new_lender_rate_bps: u16,         // 128..130
    pub did_full_fill_ask: u8,            // 130..131 — 1 if ask was removed from tree
    pub _padding: [u8; 13],               // 131..144
}
impl_discriminant!(P2PoolConvertedToFixedLog);
