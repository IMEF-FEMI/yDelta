use bytemuck::{Pod, Zeroable};
use hypertree::DataIndex;
use shank::ShankAccount;
use solana_program::{program_error::ProgramError, pubkey::Pubkey};

use crate::utils::get_discriminant;

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

pub trait Discriminant {
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

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct RiskProfileCreatedLog {
    pub global_vault: Pubkey,
    pub curator: Pubkey,
    pub profile_id: u8,

    pub _reserved0: u8,
    pub _pad0: [u8; 2],
    pub max_ltv_bps: u16,
    pub _pad1: [u8; 2],
    pub max_term_seconds: u32,
    pub _pad2: [u8; 4],
}
impl_discriminant!(RiskProfileCreatedLog);

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct RiskProfileRemovedLog {
    pub global_vault: Pubkey,
    pub curator: Pubkey,
    pub profile_id: u8,
    pub _pad0: [u8; 7],
}
impl_discriminant!(RiskProfileRemovedLog);

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
    pub profile_id: u8,
    pub _padding: [u8; 7],
}
impl_discriminant!(GlobalVaultDepositLog);

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

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod, ShankAccount)]
pub struct GlobalVaultWithdrawLog {
    pub global_vault: Pubkey,
    pub depositor: Pubkey,
    pub shares_burned: u128,
    pub profile_total_shares: u128,
    pub atoms_out: u64,
    pub profile_total_assets_atoms: u64,
    pub profile_id: u8,
    pub _padding: [u8; 15],
}
impl_discriminant!(GlobalVaultWithdrawLog);

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
