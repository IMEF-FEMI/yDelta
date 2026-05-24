use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program::invoke_signed, pubkey::Pubkey,
};

use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::vault::{
    get_helper_risk_profile, get_mut_helper_risk_profile, GlobalVaultFixed, RiskProfile,
    RiskProfileTreeReadOnly, GLOBAL_VAULT_SIGNER_SEED,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::ClaimCuratorFeeContext;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct ClaimCuratorFeeParams {
    pub profile_id: u8,
}

pub fn process_claim_curator_fee(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = ClaimCuratorFeeParams::try_from_slice(data)?;
    let ClaimCuratorFeeContext {
        payer,
        vault,
        global_vault_signer,
        global_vault_signer_bump,
        global_vault_staging,
        global_vault_integration_account,
        curator_token,
        debt_bank,
        liquidity_vault,
        bank_liquidity_vault_authority,
        bank_oracle,
        mint,
        token_program,
        marginfi_program,
        marginfi_group,
    } = ClaimCuratorFeeContext::load(accounts)?;

    let vault_key = *vault.info.key;

    let fee_atoms: u64 = {
        let vault_data = vault.info.try_borrow_data()?;
        let (fixed_bytes, dynamic) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
        let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
        let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1);
        let profile_idx = {
            let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
            tree.lookup_index(&probe)
        };
        require!(
            profile_idx != NIL,
            YdeltaError::VaultProfileNotFound,
            "profile_id {} not found",
            params.profile_id
        )?;
        let profile = get_helper_risk_profile(dynamic, profile_idx).get_value();
        require!(
            *payer.info.key == profile.curator,
            YdeltaError::VaultCuratorRequired,
            "claim_curator_fee: signer is not profile.curator"
        )?;
        profile.accumulated_curator_fee_atoms
    };

    if fee_atoms == 0 {
        return Ok(());
    }

    let vault_bytes = vault_key.to_bytes();
    let signer_bump_arr = [global_vault_signer_bump];
    let global_vault_signer_seeds: &[&[u8]] =
        &[GLOBAL_VAULT_SIGNER_SEED, &vault_bytes, &signer_bump_arr];

    let withdraw_shares: u128 =
        MarginfiV18Adapter.amount_to_asset_shares(&[debt_bank.info.clone()], fee_atoms)?;
    let withdraw_accounts: Vec<AccountInfo> = vec![
        marginfi_group.info.clone(),
        global_vault_integration_account.info.clone(),
        global_vault_signer.clone(),
        debt_bank.info.clone(),
        global_vault_staging.info.clone(),
        bank_liquidity_vault_authority.clone(),
        liquidity_vault.info.clone(),
        token_program.info.clone(),
        marginfi_program.info.clone(),
        debt_bank.info.clone(),
        bank_oracle.clone(),
    ];
    let (actual_atoms, _actual_shares_burned) = MarginfiV18Adapter.withdraw(
        &withdraw_accounts,
        withdraw_shares,
        &[global_vault_signer_seeds],
    )?;

    let payout_atoms: u64 = actual_atoms.min(fee_atoms);
    let surplus_atoms: u64 = actual_atoms.saturating_sub(payout_atoms);

    if token_program.info.key == &spl_token_2022::id() {
        let ix = spl_token_2022::instruction::transfer_checked(
            token_program.info.key,
            global_vault_staging.info.key,
            mint.info.key,
            curator_token.info.key,
            global_vault_signer.key,
            &[],
            payout_atoms,
            mint.mint.decimals,
        )?;
        invoke_signed(
            &ix,
            &[
                global_vault_staging.info.clone(),
                mint.info.clone(),
                curator_token.info.clone(),
                global_vault_signer.clone(),
                token_program.info.clone(),
            ],
            &[global_vault_signer_seeds],
        )?;
    } else {
        invoke_signed(
            &spl_token::instruction::transfer(
                token_program.info.key,
                global_vault_staging.info.key,
                curator_token.info.key,
                global_vault_signer.key,
                &[],
                payout_atoms,
            )?,
            &[
                global_vault_staging.info.clone(),
                curator_token.info.clone(),
                global_vault_signer.clone(),
                token_program.info.clone(),
            ],
            &[global_vault_signer_seeds],
        )?;
    }

    if surplus_atoms > 0 {
        let deposit_accounts: Vec<AccountInfo> = vec![
            marginfi_group.info.clone(),
            global_vault_integration_account.info.clone(),
            global_vault_signer.clone(),
            debt_bank.info.clone(),
            global_vault_staging.info.clone(),
            liquidity_vault.info.clone(),
            token_program.info.clone(),
            marginfi_program.info.clone(),
        ];
        let _credited_shares: u128 = MarginfiV18Adapter.deposit(
            &deposit_accounts,
            surplus_atoms,
            &[global_vault_signer_seeds],
        )?;
    }

    {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1);
        let profile_idx = {
            let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
            tree.lookup_index(&probe)
        };
        if profile_idx != NIL {
            let profile_node = get_mut_helper_risk_profile(dynamic, profile_idx);
            let acc = &mut profile_node.get_mut_value().accumulated_curator_fee_atoms;

            *acc = acc.saturating_sub(payout_atoms);
        }
    }

    Ok(())
}
