//! Per-instruction processor modules. Each child module exports a single
//! `pub fn process_X(program_id, accounts, data) -> ProgramResult`
//! dispatched from [`crate::process_instruction`] via the
//! [`crate::program::instruction::YdeltaInstruction`] tag. The
//! [`shared`] and [`fee_config_helpers`] modules host cross-processor
//! utilities (market expansion, fee-config validation/overrides).

pub mod admin_cancel_sub_vault_order;
pub mod admin_transfer;
pub mod cancel_order;
pub mod cancel_order_for_sub_vault;
pub mod check_liquidatable;
pub mod claim_curator_fee;
pub mod claim_repayment_for_sub_vault;
pub mod claim_seat;
pub mod convert_p2pool_to_fixed;
pub mod create_market;
pub mod create_sub_vault;
pub mod create_vault;
pub mod deposit;
pub mod fee_config_helpers;
pub mod global_config_admin;
pub mod global_vault_deposit;
pub mod global_vault_withdraw;
pub mod liquidate_loan;
pub mod match_crank;
pub mod place_order;
pub mod place_order_for_sub_vault;
pub mod process_matched_loan;
pub mod protocol_fee_claim;
pub mod remove_sub_vault;
pub mod repay;
pub mod resume_sub_vault;
pub mod set_fee_config;
pub mod set_market_pause;
pub mod set_vault_pause;
pub mod settle_matured_loan;
pub mod shared;
pub mod sunset_sub_vault;
pub mod sync_market_position;
pub mod update_order;
pub mod update_order_for_sub_vault;
pub mod update_sub_vault;
pub mod withdraw;
