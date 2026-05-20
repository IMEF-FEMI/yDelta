//! yDelta — fixed-rate / fixed-term lending protocol on a Solana
//! CLOB. The book holds only vault risk-profile asks `(rate, term)`;
//! a borrower bid `(rate, term, principal, collateral)` is an
//! immediate-or-cancel taker that crosses them into discrete loans.
//! Idle capital on either side earns marginfi supply yield via the
//! seat-share invariant.
//!
//! Lender quotes come only from risk profiles inside a `GlobalVault`
//! (curator-managed, depositor-funded). The vault profile's curator-set
//! `max_ltv_bps` cap is enforced at match time.

pub mod logs;
pub mod math;
pub mod program;
pub mod protocol;
pub mod state;
pub mod utils;
pub mod validation;

pub mod deps {
    pub use hypertree;
}

use program::{
    admin_transfer::{
        process_accept_curator, process_accept_global_vault_admin, process_accept_market_admin,
        process_transfer_curator, process_transfer_global_vault_admin,
        process_transfer_market_admin,
    },
    cancel_order_for_risk_profile::process_cancel_order_for_risk_profile,
    check_liquidatable::{process_check_ltv_liquidatable, process_check_maturity_liquidatable},
    claim_curator_fee::process_claim_curator_fee,
    claim_repayment_for_risk_profile::process_claim_repayment_for_risk_profile,
    claim_seat::process_claim_seat,
    convert_p2pool_to_fixed::process_convert_p2pool_to_fixed,
    create_market::process_create_market,
    create_risk_profile::process_create_risk_profile,
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
    place_order_for_risk_profile::process_place_order_for_risk_profile,
    process_matched_loan::process_process_matched_loan,
    protocol_fee_claim::process_protocol_fee_claim,
    repay::process_repay,
    set_fee_config::process_set_fee_config,
    set_market_pause::process_set_market_pause,
    set_vault_pause::process_set_vault_pause,
    settle_matured_loan::process_settle_matured_loan,
    sync_market_position::process_sync_market_position,
    update_order_for_risk_profile::process_update_order_for_risk_profile,
    update_risk_profile::process_update_risk_profile,
    withdraw::process_withdraw,
    YdeltaInstruction,
};
use solana_program::{
    account_info::AccountInfo, declare_id, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

#[cfg(not(feature = "no-entrypoint"))]
use solana_security_txt::security_txt;

// TODO(pre-mainnet): fill `project_url`, `contacts`, `policy`,
// `source_code` with real values before deploy. Empty fields fail the
// solana-security-txt spec's "useful" check and weaken incident-response
// posture.
#[cfg(not(feature = "no-entrypoint"))]
security_txt! {
    name: "ydelta",
    project_url: "https://ydelta.xyz",
    contacts: "bolajifemi28@gmail.com",
    policy: "",
    preferred_languages: "en",
    source_code: "https://github.com/imef-femi/yDelta"
}

declare_id!("A1fNwJV5C2BTKWcnHmaELNq2TLB11UP7mp9P7q4ahWnu");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
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
        YdeltaInstruction::CreateRiskProfile => {
            process_create_risk_profile(program_id, accounts, data)?
        }
        YdeltaInstruction::GlobalVaultDeposit => {
            process_global_vault_deposit(program_id, accounts, data)?
        }
        YdeltaInstruction::GlobalVaultWithdraw => {
            process_global_vault_withdraw(program_id, accounts, data)?
        }
        YdeltaInstruction::PlaceOrderForRiskProfile => {
            process_place_order_for_risk_profile(program_id, accounts, data)?
        }
        YdeltaInstruction::CancelOrderForRiskProfile => {
            process_cancel_order_for_risk_profile(program_id, accounts, data)?
        }
        YdeltaInstruction::UpdateOrderForRiskProfile => {
            process_update_order_for_risk_profile(program_id, accounts, data)?
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
        YdeltaInstruction::ClaimRepaymentForRiskProfile => {
            process_claim_repayment_for_risk_profile(program_id, accounts, data)?
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
        YdeltaInstruction::UpdateRiskProfile => {
            process_update_risk_profile(program_id, accounts, data)?
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
    }

    Ok(())
}
