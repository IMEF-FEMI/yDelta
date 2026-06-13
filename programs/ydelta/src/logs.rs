//! On-chain event log structs emitted via `sol_log_data` for off-chain
//! indexers. Each event is `repr(C)` + `Pod`, prefixed with the 8-byte
//! `Discriminant` returned by `keccak(program_id || type_name)[..8]`
//! (see [`crate::utils::get_discriminant`]) so the indexer can dispatch
//! by tag without needing IDL metadata.

use bytemuck::{Pod, Zeroable};
use hypertree::DataIndex;
use shank::ShankAccount;
use solana_program::{program_error::ProgramError, pubkey::Pubkey};

use crate::utils::get_discriminant;

/// Emit `e` to the transaction log as `[discriminant(8) | payload]`.
/// Marked `#[inline(never)]` to keep the 3000-byte buffer off any
/// caller's stack. Compile-time assert blocks any `T` whose serialized
/// size (with the discriminant) would exceed the buffer.
#[inline(never)]
pub fn emit_stack<T: bytemuck::Pod + Discriminant>(e: T) -> Result<(), ProgramError> {
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

/// 8-byte tag identifying an event variant on the wire. The
/// `impl_discriminant!` macro implements this by hashing the type's
/// stringified name with the program id.
pub trait Discriminant {
    /// Stable little-endian 8-byte tag for the implementing type.
    fn discriminant() -> [u8; 8];
}

macro_rules! impl_discriminant {
    ($t:ident) => {
        impl Discriminant for $t {
            fn discriminant() -> [u8; 8] {
                u64::to_le_bytes(get_discriminant(stringify!($t)))
            }
        }
    };
}

/// Emitted from `process_create_market` after the `MarketFixed` PDA is
/// initialized.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct CreateMarketLog {
    pub market: Pubkey,
    pub creator: Pubkey,
    pub debt_mint: Pubkey,
    pub collateral_mint: Pubkey,
}
impl_discriminant!(CreateMarketLog);

/// Emitted from `process_claim_seat` after a `ClaimedSeat` is inserted
/// into the market's seat tree.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct ClaimSeatLog {
    pub market: Pubkey,
    pub trader: Pubkey,
}
impl_discriminant!(ClaimSeatLog);

/// Emitted from `process_deposit` after seat balances are credited.
/// `mint` distinguishes debt-side vs collateral-side deposits.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct DepositLog {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub mint: Pubkey,
    pub amount_atoms: u64,
}
impl_discriminant!(DepositLog);

/// Emitted from `process_withdraw` after seat balances are debited and
/// atoms transferred to the trader's wallet ATA.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct WithdrawLog {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub mint: Pubkey,
    pub amount_atoms: u64,
}
impl_discriminant!(WithdrawLog);

/// Emitted from `process_place_order` for each accepted order before
/// matching runs. `order_type` distinguishes bid/ask/post-only variants;
/// `side` is the borrower/lender flag.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct OrderPlacedLog {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub trader_seat_index: DataIndex,
    pub side: u8,

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

/// Emitted from the matching engine for each cross that produces a
/// queued `MatchedLoan` node (one per fill, before cranker promotion
/// to a `LoanFixed` PDA).
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

/// Emitted when the matching engine reaps an order whose
/// `last_valid_unix_ts` has passed.
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

/// Emitted when an order's unfilled residual rests on the book after
/// matching. `principal_remaining_atoms` is the size after the fill.
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

/// Emitted when an IOC (immediate-or-cancel) order drops its unfilled
/// residual instead of resting. `principal_dropped_atoms` is the
/// discarded size.
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

/// Emitted from order-cancellation paths (both trader-side and
/// sub-vault-side) identifying the removed resting order by sequence.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct CancelOrderLog {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub sequence: u64,
}
impl_discriminant!(CancelOrderLog);

/// Emitted from `UpdateOrderForSubVault` (cancel-and-replace) so the
/// indexer can link the retired `old_sequence` to the fresh `new_sequence`.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct UpdateOrderLog {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub old_sequence: u64,
    pub new_sequence: u64,
}
impl_discriminant!(UpdateOrderLog);

/// Emitted from `process_process_matched_loan` after a `MatchedLoan`
/// node is promoted to a `LoanFixed` PDA and the lender has been
/// credited the principal/origination split.
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

/// Emitted from `process_repay` for each repay step. `full_repay = 1`
/// indicates the loan PDA closed in the same call.
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

/// Emitted from the lender-side claim sweeper (`ClaimSeat` repayment
/// path). `closed = 1` indicates the loan was fully drained and
/// removed. `protocol_fee_shares_swept` is the marginfi-asset-share
/// slice diverted to `accumulated_protocol_fee_shares`.
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

/// Emitted from `process_claim_repayment_for_sub_vault` per sweep
/// from the lender_marginfi_account into the sub-vault's idle pool.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct RepaymentClaimedForSubVaultLog {
    pub market: Pubkey,
    pub loan: Pubkey,
    pub global_vault: Pubkey,
    pub sub_vault_id: u16,
    pub _pad0: [u8; 6],
    pub claimed_atoms: u64,
    pub principal_atoms: u64,
    pub _pad1: [u8; 8],
    pub protocol_fee_shares_swept: u128,
}
impl_discriminant!(RepaymentClaimedForSubVaultLog);

/// Emitted from `process_create_vault` after the `GlobalVaultFixed` PDA
/// and its marginfi integration account are initialized.
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

/// Emitted from `process_create_sub_vault` after the sub-vault is
/// appended to the vault's `sub_vaults` tree.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct SubVaultCreatedLog {
    pub global_vault: Pubkey,
    pub curator: Pubkey,
    pub sub_vault_id: u16,

    /// `SUB_VAULT_KIND_POOL` or `SUB_VAULT_KIND_PRIVATE`.
    pub kind: u8,
    pub _pad0: [u8; 1],
    pub max_ltv_bps: u16,
    pub liquidation_ltv_bps: u16,
    pub max_term_seconds: u32,
    pub spread_bps: u16,
    pub curator_fee_bps: u16,
}
impl_discriminant!(SubVaultCreatedLog);

/// Emitted from `process_remove_sub_vault` after a sunset sub-vault is
/// removed from the vault tree.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct SubVaultRemovedLog {
    pub global_vault: Pubkey,
    pub curator: Pubkey,
    pub sub_vault_id: u16,
    pub _pad0: [u8; 6],
}
impl_discriminant!(SubVaultRemovedLog);

/// Emitted from `process_global_vault_deposit`. `shares_minted` and
/// `gain_atoms` reflect the pre-mint snapshot accrual; the totals are
/// post-deposit.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct GlobalVaultDepositLog {
    pub global_vault: Pubkey,
    pub depositor: Pubkey,
    pub shares_minted: u128,
    pub profile_total_shares: u128,
    pub atoms_in: u64,
    pub gain_atoms: u64,
    pub profile_total_assets_atoms: u64,
    pub sub_vault_id: u16,
    pub _padding: [u8; 6],
}
impl_discriminant!(GlobalVaultDepositLog);

/// Emitted from `process_place_order_for_sub_vault` whenever a
/// curator rests a vault ask on a market.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct PlaceOrderForSubVaultLog {
    pub global_vault: Pubkey,
    pub market: Pubkey,
    pub sub_vault_id: u16,
    pub side: u8,
    pub _pad0: [u8; 5],
    pub rate_bps: u16,
    pub _pad1: [u8; 2],
    pub term_seconds: u32,
    pub order_sequence_in_market: u64,
}
impl_discriminant!(PlaceOrderForSubVaultLog);

/// Emitted by the permissionless `MatchCrank`.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct MatchCrankLog {
    pub market: Pubkey,
    pub cranker: Pubkey,
    pub fills: u32,
    pub _pad0: [u8; 4],
}
impl_discriminant!(MatchCrankLog);

/// Emitted when a borrower cancels their resting bid.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct OrderCanceledLog {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub sequence: u64,
    pub side: u8,
    pub _pad0: [u8; 7],
}
impl_discriminant!(OrderCanceledLog);

/// Emitted when the matching engine skips an ask whose stored rate has
/// fallen below the live marginfi lending APR (fill-time floor).
/// The curator re-syncs with a parameterless `update_order_for_sub_vault`.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct AskSkippedBelowFloorLog {
    pub market: Pubkey,
    pub sub_vault_id: u16,
    pub ask_rate_bps: u16,
    pub floor_bps: u16,
    pub _pad0: [u8; 2],
    pub order_sequence: u64,
}
impl_discriminant!(AskSkippedBelowFloorLog);

/// Emitted from `process_cancel_order_for_sub_vault` and from the
/// admin-cancel + update-order paths. `is_replace = 1` indicates the
/// cancel half of a cancel-and-replace.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct CancelOrderForSubVaultLog {
    pub global_vault: Pubkey,
    pub market: Pubkey,
    pub sub_vault_id: u16,
    pub is_replace: u8,
    pub _pad0: [u8; 5],
    pub order_sequence_in_market: u64,
}
impl_discriminant!(CancelOrderForSubVaultLog);

/// Emitted from `process_global_vault_withdraw` after sub-vault shares
/// are burned and atoms returned to the depositor.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct GlobalVaultWithdrawLog {
    pub global_vault: Pubkey,
    pub depositor: Pubkey,
    pub shares_burned: u128,
    pub profile_total_shares: u128,
    pub atoms_out: u64,
    pub profile_total_assets_atoms: u64,
    pub sub_vault_id: u16,
    pub _padding: [u8; 14],
}
impl_discriminant!(GlobalVaultWithdrawLog);

/// Emitted from `process_liquidate_loan` and `process_settle_matured_loan`.
/// `liquidation_kind` distinguishes LTV vs maturity triggers;
/// `is_partial = 1` indicates the loan PDA survived the call.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct LoanLiquidatedLog {
    pub market: Pubkey,
    pub loan: Pubkey,
    pub liquidator: Pubkey,
    pub debt_paid_atoms: u64,
    pub collateral_seized_atoms: u64,
    pub liquidation_kind: u8,

    pub is_partial: u8,
    pub _padding: [u8; 14],
}
impl_discriminant!(LoanLiquidatedLog);

/// Emitted from a liquidation path when seized collateral was
/// insufficient to cover the live debt + bonus. `gap_collateral_atoms`
/// is the shortfall, `debt_atoms_remaining` is the unrecoverable
/// liability written off.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct BadDebtLog {
    pub market: Pubkey,
    pub loan: Pubkey,

    pub gap_collateral_atoms: u64,
    pub debt_atoms_remaining: u64,
    pub _padding: [u8; 16],
}
impl_discriminant!(BadDebtLog);

/// Emitted from `process_convert_p2pool_to_fixed` per cross — one log
/// per matched vault ask, regardless of whether the borrower's
/// P2Pool balance went to zero in the same call.
#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct P2PoolConvertedToFixedLog {
    pub market: Pubkey,
    pub loan: Pubkey,
    pub borrower: Pubkey,
    pub new_lender_seat_index: DataIndex,
    pub _pad0: [u8; 4],
    pub matched_principal_atoms: u64,
    pub borrow_shares_burned: u128,
    pub new_lender_rate_bps: u16,
    pub did_full_fill_ask: u8,
    pub _padding: [u8; 13],
}
impl_discriminant!(P2PoolConvertedToFixedLog);
