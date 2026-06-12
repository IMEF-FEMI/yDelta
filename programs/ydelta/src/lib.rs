//! yDelta on-chain program — entrypoint and module roots.
//!
//! Top-level layout:
//! - [`state`] — on-chain account data types (markets, vaults, loans,
//!   sub-vaults, seats, orders) and the helpers that mutate them.
//! - [`program`] — instruction tag enum, error enum, processor handlers
//!   (one per instruction), and client-side instruction builders.
//! - [`protocol`] — adapters to external programs (marginfi v0.1.8 CPIs,
//!   Pyth / Switchboard oracle decoders).
//! - [`validation`] — account-list loaders that build typed contexts
//!   from raw `&[AccountInfo]` for each instruction.
//! - [`math`] — `Fp48` fixed-point wrapper plus `mul_div` / `mul_scale`
//!   / `div_scale` helpers that funnel every multiplication through
//!   `U256` to avoid the `u128` overflow class.
//! - [`logs`] — `Pod` structs emitted via `emit_stack` for off-chain
//!   indexers; each variant is keyed by an 8-byte discriminant.
//! - [`utils`] — small cross-module helpers (PDA account creation, etc).

pub mod logs;
pub mod math;
pub mod program;
pub mod protocol;
pub mod state;
pub mod utils;
pub mod validation;

/// Re-exports of upstream crates so SDK consumers can pull them through
/// the program crate instead of pinning a separate `hypertree` version.
pub mod deps {
    pub use hypertree;
}

use program::{
    admin_cancel_sub_vault_order::process_admin_cancel_sub_vault_order,
    admin_transfer::{
        process_accept_curator, process_accept_global_vault_admin, process_accept_market_admin,
        process_transfer_curator, process_transfer_global_vault_admin,
        process_transfer_market_admin,
    },
    cancel_order_for_sub_vault::process_cancel_order_for_sub_vault,
    check_liquidatable::{process_check_ltv_liquidatable, process_check_maturity_liquidatable},
    claim_curator_fee::process_claim_curator_fee,
    claim_repayment_for_sub_vault::process_claim_repayment_for_sub_vault,
    claim_seat::process_claim_seat,
    convert_p2pool_to_fixed::process_convert_p2pool_to_fixed,
    create_market::process_create_market,
    create_sub_vault::{process_create_pool_sub_vault, process_create_private_sub_vault},
    create_vault::process_create_vault,
    deposit::process_deposit,
    global_config_admin::{
        process_accept_protocol_admin, process_create_global_config, process_set_global_pause,
        process_transfer_protocol_admin,
    },
    global_vault_deposit::process_global_vault_deposit,
    global_vault_withdraw::process_global_vault_withdraw,
    liquidate_loan::process_liquidate_loan,
    place_order::process_place_order,
    place_order_for_sub_vault::process_place_order_for_sub_vault,
    process_matched_loan::process_process_matched_loan,
    protocol_fee_claim::process_protocol_fee_claim,
    remove_sub_vault::process_remove_sub_vault,
    repay::process_repay,
    resume_sub_vault::process_resume_sub_vault,
    set_fee_config::process_set_fee_config,
    set_market_pause::process_set_market_pause,
    set_vault_pause::process_set_vault_pause,
    settle_matured_loan::process_settle_matured_loan,
    sunset_sub_vault::process_sunset_sub_vault,
    sync_market_position::process_sync_market_position,
    update_order_for_sub_vault::process_update_order_for_sub_vault,
    update_sub_vault::process_update_sub_vault,
    withdraw::process_withdraw,
    YdeltaInstruction,
};
use solana_program::{
    account_info::AccountInfo, declare_id, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

#[cfg(not(feature = "no-entrypoint"))]
use solana_security_txt::security_txt;

#[cfg(not(feature = "no-entrypoint"))]
security_txt! {
    name: "ydelta",
    project_url: "https://ydelta.xyz",
    contacts: "bolajifemi28@gmail.com",
    policy: "",
    preferred_languages: "en",
    source_code: "https://github.com/imef-femi/yDelta"
}

declare_id!("Ar38x7KobxmwvKwSL2VzdjFxrtXd3Gs2VfB3ZCakyJas");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

/// Program entrypoint. Reads the first instruction-data byte as a
/// [`YdeltaInstruction`] tag, then dispatches to the matching processor
/// in [`program::processor`]. Hard-rejects if `program_id` is not this
/// program's `crate::id()`.
pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    if program_id != &crate::id() {
        return Err(ProgramError::IncorrectProgramId);
    }

    let (tag, data) = instruction_data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;

    let instruction: YdeltaInstruction =
        YdeltaInstruction::try_from(*tag).or(Err(ProgramError::InvalidInstructionData))?;

    match instruction {
        YdeltaInstruction::CreateMarket => process_create_market(program_id, accounts, data)?,
        YdeltaInstruction::ClaimSeat => process_claim_seat(program_id, accounts, data)?,
        YdeltaInstruction::Deposit => process_deposit(program_id, accounts, data)?,
        YdeltaInstruction::Withdraw => process_withdraw(program_id, accounts, data)?,
        YdeltaInstruction::PlaceOrder => process_place_order(program_id, accounts, data)?,
        YdeltaInstruction::ProcessMatchedLoan => {
            process_process_matched_loan(program_id, accounts, data)?
        }
        YdeltaInstruction::Repay => process_repay(program_id, accounts, data)?,
        YdeltaInstruction::SyncMarketPosition => {
            process_sync_market_position(program_id, accounts, data)?
        }
        YdeltaInstruction::CreateVault => process_create_vault(program_id, accounts, data)?,
        YdeltaInstruction::CreatePoolSubVault => {
            process_create_pool_sub_vault(program_id, accounts, data)?
        }
        YdeltaInstruction::CreatePrivateSubVault => {
            process_create_private_sub_vault(program_id, accounts, data)?
        }
        YdeltaInstruction::GlobalVaultDeposit => {
            process_global_vault_deposit(program_id, accounts, data)?
        }
        YdeltaInstruction::GlobalVaultWithdraw => {
            process_global_vault_withdraw(program_id, accounts, data)?
        }
        YdeltaInstruction::PlaceOrderForSubVault => {
            process_place_order_for_sub_vault(program_id, accounts, data)?
        }
        YdeltaInstruction::CancelOrderForSubVault => {
            process_cancel_order_for_sub_vault(program_id, accounts, data)?
        }
        YdeltaInstruction::UpdateOrderForSubVault => {
            process_update_order_for_sub_vault(program_id, accounts, data)?
        }
        YdeltaInstruction::ClaimCuratorFee => {
            process_claim_curator_fee(program_id, accounts, data)?
        }
        YdeltaInstruction::SettleMaturedLoan => {
            process_settle_matured_loan(program_id, accounts, data)?
        }
        YdeltaInstruction::LiquidateLoan => process_liquidate_loan(program_id, accounts, data)?,
        YdeltaInstruction::SetFeeConfig => process_set_fee_config(program_id, accounts, data)?,
        YdeltaInstruction::ProtocolFeeClaim => {
            process_protocol_fee_claim(program_id, accounts, data)?
        }
        YdeltaInstruction::ClaimRepaymentForSubVault => {
            process_claim_repayment_for_sub_vault(program_id, accounts, data)?
        }
        YdeltaInstruction::TransferMarketAdmin => {
            process_transfer_market_admin(program_id, accounts, data)?
        }
        YdeltaInstruction::AcceptMarketAdmin => {
            process_accept_market_admin(program_id, accounts, data)?
        }
        YdeltaInstruction::TransferGlobalVaultAdmin => {
            process_transfer_global_vault_admin(program_id, accounts, data)?
        }
        YdeltaInstruction::AcceptGlobalVaultAdmin => {
            process_accept_global_vault_admin(program_id, accounts, data)?
        }
        YdeltaInstruction::TransferCurator => process_transfer_curator(program_id, accounts, data)?,
        YdeltaInstruction::AcceptCurator => process_accept_curator(program_id, accounts, data)?,
        YdeltaInstruction::SetMarketPause => process_set_market_pause(program_id, accounts, data)?,
        YdeltaInstruction::CreateGlobalConfig => {
            process_create_global_config(program_id, accounts, data)?
        }
        YdeltaInstruction::TransferProtocolAdmin => {
            process_transfer_protocol_admin(program_id, accounts, data)?
        }
        YdeltaInstruction::AcceptProtocolAdmin => {
            process_accept_protocol_admin(program_id, accounts, data)?
        }
        YdeltaInstruction::SetGlobalPause => process_set_global_pause(program_id, accounts, data)?,
        YdeltaInstruction::UpdateSubVault => {
            process_update_sub_vault(program_id, accounts, data)?
        }
        YdeltaInstruction::ConvertP2PoolToFixed => {
            process_convert_p2pool_to_fixed(program_id, accounts, data)?
        }
        YdeltaInstruction::CheckLtvLiquidatable => {
            process_check_ltv_liquidatable(program_id, accounts, data)?
        }
        YdeltaInstruction::CheckMaturityLiquidatable => {
            process_check_maturity_liquidatable(program_id, accounts, data)?
        }
        YdeltaInstruction::SetVaultPause => process_set_vault_pause(program_id, accounts, data)?,
        YdeltaInstruction::RemoveSubVault => {
            process_remove_sub_vault(program_id, accounts, data)?
        }
        YdeltaInstruction::SunsetSubVault => {
            process_sunset_sub_vault(program_id, accounts, data)?
        }
        YdeltaInstruction::ResumeSubVault => {
            process_resume_sub_vault(program_id, accounts, data)?
        }
        YdeltaInstruction::AdminCancelSubVaultOrder => {
            process_admin_cancel_sub_vault_order(program_id, accounts, data)?
        }
    }

    Ok(())
}
