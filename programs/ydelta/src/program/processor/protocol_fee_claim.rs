//! `ProtocolFeeClaim` — sweep accumulated protocol fee shares from a market
//! into the admin's debt-token account. Gated to the market admin via
//! `ProtocolFeeClaimContext`. Zeroes `accumulated_protocol_fee_shares`,
//! withdraws the corresponding atoms from the lender marginfi integration
//! account, pays the admin via the market signer, and re-deposits any
//! marginfi rounding surplus back into the pool.

use std::cell::RefMut;

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program::invoke_signed, pubkey::Pubkey,
};

use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::state::market::MarketFixed;
use crate::validation::loaders::ProtocolFeeClaimContext;
use crate::validation::MARKET_SIGNER_SEED;

use super::shared::get_mut_dynamic_account;

/// Drain the market's accumulated protocol fees to the admin debt-token
/// account. No-op if the accumulator is zero.
pub fn process_protocol_fee_claim(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let ProtocolFeeClaimContext {
        payer: _,
        market,
        admin_debt_token,
        market_debt_vault,
        market_signer,
        market_signer_bump,
        lender_marginfi_account,
        debt_bank,
        debt_liquidity_vault,
        debt_bank_lva,
        debt_oracle_ais,
        debt_mint,
        token_program,
        marginfi_group,
        marginfi_program,
    } = ProtocolFeeClaimContext::load(accounts)?;

    let market_key = *market.info.key;

    let fee_shares: u128 = {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let snapshot = da.fixed.accumulated_protocol_fee_shares;
        da.fixed.accumulated_protocol_fee_shares = 0;
        snapshot
    };

    if fee_shares == 0 {
        return Ok(());
    }
    let fee_atoms = MarginfiV18Adapter.shares_to_amount(&[debt_bank.info.clone()], fee_shares)?;

    let market_signer_seeds: &[&[u8]] = &[
        MARKET_SIGNER_SEED,
        market_key.as_ref(),
        &[market_signer_bump],
    ];
    let mut withdraw_accounts: Vec<AccountInfo> = vec![
        marginfi_group.info.clone(),
        lender_marginfi_account.info.clone(),
        market_signer.clone(),
        debt_bank.info.clone(),
        market_debt_vault.info.clone(),
        debt_bank_lva.clone(),
        debt_liquidity_vault.info.clone(),
        token_program.info.clone(),
        marginfi_program.info.clone(),
        debt_bank.info.clone(),
    ];
    for oracle_ai in &debt_oracle_ais.ais {
        withdraw_accounts.push((*oracle_ai).clone());
    }
    let (actual_atoms, _actual_shares_burned) =
        MarginfiV18Adapter.withdraw(&withdraw_accounts, fee_shares, &[market_signer_seeds])?;

    let payout_atoms: u64 = actual_atoms.min(fee_atoms);
    let surplus_atoms: u64 = actual_atoms.saturating_sub(payout_atoms);

    if token_program.info.key == &spl_token_2022::id() {
        let ix = spl_token_2022::instruction::transfer_checked(
            token_program.info.key,
            market_debt_vault.info.key,
            debt_mint.info.key,
            admin_debt_token.info.key,
            market_signer.key,
            &[],
            payout_atoms,
            debt_mint.mint.decimals,
        )?;
        invoke_signed(
            &ix,
            &[
                market_debt_vault.info.clone(),
                debt_mint.info.clone(),
                admin_debt_token.info.clone(),
                market_signer.clone(),
                token_program.info.clone(),
            ],
            &[market_signer_seeds],
        )?;
    } else {
        let ix = spl_token::instruction::transfer(
            token_program.info.key,
            market_debt_vault.info.key,
            admin_debt_token.info.key,
            market_signer.key,
            &[],
            payout_atoms,
        )?;
        invoke_signed(
            &ix,
            &[
                market_debt_vault.info.clone(),
                admin_debt_token.info.clone(),
                market_signer.clone(),
                token_program.info.clone(),
            ],
            &[market_signer_seeds],
        )?;
    }

    if surplus_atoms > 0 {
        let deposit_accounts: Vec<AccountInfo> = vec![
            marginfi_group.info.clone(),
            lender_marginfi_account.info.clone(),
            market_signer.clone(),
            debt_bank.info.clone(),
            market_debt_vault.info.clone(),
            debt_liquidity_vault.info.clone(),
            token_program.info.clone(),
            marginfi_program.info.clone(),
        ];
        let _returned_shares: u128 =
            MarginfiV18Adapter.deposit(&deposit_accounts, surplus_atoms, &[market_signer_seeds])?;
    }

    Ok(())
}
