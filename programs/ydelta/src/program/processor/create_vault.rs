//! `CreateVault` instruction. Permissionless one-shot initializer for
//! a per-bank `GlobalVaultFixed` PDA, its staging token account, and
//! its marginfi integration account. First caller becomes
//! `global_vault_admin`. Emits `VaultCreatedLog`.

use std::cell::RefMut;

use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    program::{invoke, invoke_signed},
    program_pack::Pack,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction,
    sysvar::Sysvar,
};
use spl_token_2022::extension::ExtensionType;

use crate::logs::{emit_stack, VaultCreatedLog};
use crate::state::vault::{
    GlobalVaultFixed, GLOBAL_VAULT_SIGNER_SEED, VAULT_INTEGRATION_SEED, VAULT_SEED,
    VAULT_STAGING_SEED,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::CreateVaultContext;

use super::create_market::assert_supported_mint_extensions;

/// Initialize a global vault for one debt mint. Creates the vault PDA,
/// the staging SPL token account (signer = `global_vault_signer`), and
/// the marginfi integration account, then writes the `GlobalVaultFixed`
/// header with the signer as `global_vault_admin`.
pub fn process_create_vault(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let CreateVaultContext {
        payer,
        vault,
        vault_bump,
        mint,
        global_vault_signer,
        global_vault_signer_bump,
        integration_account,
        integration_account_bump,
        global_vault_staging,
        global_vault_staging_bump,
        token_program,
        token_program_22,
        marginfi_group,
        lending_pool,
        marginfi_program,
        system_program,
    } = CreateVaultContext::load(accounts)?;

    assert_supported_mint_extensions(&mint)?;

    let mint_key = *mint.info.key;
    let vault_key = *vault.key;
    let vault_bytes = vault_key.to_bytes();

    let rent: Rent = Rent::get()?;
    // v1 D1: the vault PDA is keyed by the marginfi bank, not the mint.
    let bank_bytes = lending_pool.info.key.to_bytes();
    let vault_bump_arr = [vault_bump];
    let vault_seeds: &[&[u8]] = &[VAULT_SEED, &bank_bytes, &vault_bump_arr];
    create_pda_resilient(
        payer.info,
        &vault,
        system_program.info,
        GLOBAL_VAULT_FIXED_SIZE,
        &crate::id(),
        vault_seeds,
        &rent,
    )?;

    let mint_owner = mint.info.owner;
    let token_prog_for_mint = if mint_owner == &spl_token_2022::id() {
        token_program_22.info.clone()
    } else {
        token_program.info.clone()
    };
    let staging_space = if mint_owner == &spl_token_2022::id() {
        ExtensionType::try_calculate_account_len::<spl_token_2022::state::Account>(&[])?
    } else {
        spl_token::state::Account::LEN
    };
    let staging_bump_arr = [global_vault_staging_bump];
    let staging_seeds: &[&[u8]] = &[VAULT_STAGING_SEED, &vault_bytes, &staging_bump_arr];
    create_pda_resilient(
        payer.info,
        &global_vault_staging,
        system_program.info,
        staging_space,
        token_prog_for_mint.key,
        staging_seeds,
        &rent,
    )?;

    let init_staging_ix = if mint_owner == &spl_token_2022::id() {
        spl_token_2022::instruction::initialize_account3(
            &spl_token_2022::id(),
            global_vault_staging.key,
            mint.info.key,
            global_vault_signer.key,
        )?
    } else {
        spl_token::instruction::initialize_account3(
            &spl_token::id(),
            global_vault_staging.key,
            mint.info.key,
            global_vault_signer.key,
        )?
    };
    invoke(
        &init_staging_ix,
        &[
            global_vault_staging.clone(),
            mint.info.clone(),
            token_prog_for_mint.clone(),
        ],
    )?;

    let integration_bump_arr = [integration_account_bump];
    let integration_seeds: &[&[u8]] =
        &[VAULT_INTEGRATION_SEED, &vault_bytes, &integration_bump_arr];
    let signer_bump_arr = [global_vault_signer_bump];
    let global_vault_signer_seeds: &[&[u8]] =
        &[GLOBAL_VAULT_SIGNER_SEED, &vault_bytes, &signer_bump_arr];

    let init_ix = marginfi_mocks::initialize_marginfi_account_ix(
        &marginfi_mocks::InitializeMarginfiAccounts {
            marginfi_group: *marginfi_group.info.key,
            marginfi_account: *integration_account.key,
            authority: *global_vault_signer.key,
            fee_payer: *payer.info.key,
            system_program: *system_program.info.key,
        },
    );
    invoke_signed(
        &init_ix,
        &[
            marginfi_group.info.clone(),
            integration_account.clone(),
            global_vault_signer.clone(),
            payer.info.clone(),
            system_program.info.clone(),
            marginfi_program.info.clone(),
        ],
        &[integration_seeds, global_vault_signer_seeds],
    )?;

    {
        let data: &mut RefMut<&mut [u8]> = &mut vault.try_borrow_mut_data()?;
        let header: &mut GlobalVaultFixed =
            bytemuck::from_bytes_mut(&mut data[..GLOBAL_VAULT_FIXED_SIZE]);
        *header = GlobalVaultFixed::new_empty(
            mint_key,
            *payer.info.key,
            *marginfi_group.info.key,
            *integration_account.key,
            *global_vault_signer.key,
            global_vault_signer_bump,
            *lending_pool.info.key,
        );
    }

    emit_stack(VaultCreatedLog {
        global_vault: vault_key,
        mint: mint_key,
        global_vault_admin: *payer.info.key,
        integration_pool: *marginfi_group.info.key,
        integration_account: *integration_account.key,
        global_vault_signer: *global_vault_signer.key,
    })?;

    Ok(())
}

fn create_pda_resilient<'info>(
    payer: &AccountInfo<'info>,
    target: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    space: usize,
    owner: &Pubkey,
    signer_seeds: &[&[u8]],
    rent: &Rent,
) -> ProgramResult {
    let required = rent.minimum_balance(space);
    let current = target.lamports();
    if current < required {
        invoke(
            &system_instruction::transfer(payer.key, target.key, required - current),
            &[payer.clone(), target.clone(), system_program.clone()],
        )?;
    }
    invoke_signed(
        &system_instruction::allocate(target.key, space as u64),
        &[target.clone(), system_program.clone()],
        &[signer_seeds],
    )?;
    invoke_signed(
        &system_instruction::assign(target.key, owner),
        &[target.clone(), system_program.clone()],
        &[signer_seeds],
    )?;
    Ok(())
}
