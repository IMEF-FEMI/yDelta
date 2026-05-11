//! Initialize a `GlobalVault` for a mint.

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

    // Reject T22 mints with non-trivial extensions (mirror create_market).
    assert_supported_mint_extensions(&mint)?;

    let mint_key = *mint.info.key;
    let vault_key = *vault.key;
    let vault_bytes = vault_key.to_bytes();

    // Allocate the vault PDA via system_program::create_account.
    let rent: Rent = Rent::get()?;
    let lamports = rent.minimum_balance(GLOBAL_VAULT_FIXED_SIZE);
    let mint_bytes = mint_key.to_bytes();
    let vault_bump_arr = [vault_bump];
    let vault_seeds: &[&[u8]] = &[VAULT_SEED, &mint_bytes, &vault_bump_arr];
    invoke_signed(
        &system_instruction::create_account(
            payer.info.key,
            &vault_key,
            lamports,
            GLOBAL_VAULT_FIXED_SIZE as u64,
            &crate::id(),
        ),
        &[
            payer.info.clone(),
            vault.clone(),
            system_program.info.clone(),
        ],
        &[vault_seeds],
    )?;

    // Allocate the per-vault SPL staging token account.
    // Owned by `global_vault_signer` so marginfi.deposit / .withdraw CPIs (which
    // require source/destination to be authority-owned) can use it.
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
    let staging_lamports = rent.minimum_balance(staging_space);
    let staging_bump_arr = [global_vault_staging_bump];
    let staging_seeds: &[&[u8]] = &[VAULT_STAGING_SEED, &vault_bytes, &staging_bump_arr];
    invoke_signed(
        &system_instruction::create_account(
            payer.info.key,
            global_vault_staging.key,
            staging_lamports,
            staging_space as u64,
            token_prog_for_mint.key,
        ),
        &[
            payer.info.clone(),
            global_vault_staging.clone(),
            system_program.info.clone(),
        ],
        &[staging_seeds],
    )?;
    // Initialize the SPL token account with global_vault_signer as authority.
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

    // Initialize the integration_account in marginfi.
    // The integration_account is a PDA of the vault; the new account
    // signs via its own seeds, and the authority (global_vault_signer) signs
    // via its seeds. Marginfi's inner system::create_account sees both
    // signed.
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

    // Stamp the GlobalVaultFixed header.
    {
        let data: &mut RefMut<&mut [u8]> = &mut vault.try_borrow_mut_data()?;
        let header: &mut GlobalVaultFixed =
            bytemuck::from_bytes_mut(&mut data[..GLOBAL_VAULT_FIXED_SIZE]);
        *header = GlobalVaultFixed::new_empty(
            mint_key,
            *payer.info.key, // global_vault_admin = signer (first caller)
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
