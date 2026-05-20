use std::cell::RefMut;

use hypertree::get_mut_helper;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, msg, program_error::ProgramError,
    program_pack::Pack, pubkey::Pubkey, rent::Rent, sysvar::Sysvar,
};
use spl_token_2022::extension::{BaseStateWithExtensions, ExtensionType, StateWithExtensions};

use crate::logs::{emit_stack, CreateMarketLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::MarketFixed;
use crate::utils::create_account;
use crate::validation::loaders::CreateMarketContext;
use crate::validation::{
    MintAccountInfo, MARGINFI_BORROWER_ACCOUNT_SEED, MARGINFI_LENDER_ACCOUNT_SEED,
    MARKET_SIGNER_SEED,
};

use super::shared::{expand_market, invoke};

pub fn process_create_market(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let CreateMarketContext {
        payer,
        market,
        system_program,
        debt_mint,
        collateral_mint,
        debt_vault,
        collateral_vault,
        token_program,
        token_program_22,
        marginfi_group,
        debt_bank,
        collateral_bank,
        lender_marginfi_account,
        lender_marginfi_account_bump,
        borrower_marginfi_account,
        borrower_marginfi_account_bump,
        market_signer,
        market_signer_bump,
        marginfi_program,
    } = CreateMarketContext::load(accounts)?;

    // Business-rule check: the two mints must differ. Account/pubkey
    // validations on the marginfi side were already enforced by the
    // typed wrappers in `CreateMarketContext::load`.
    require!(
        debt_mint.info.key != collateral_mint.info.key,
        YdeltaError::InvalidMarketParameters,
        "debt_mint and collateral_mint must differ"
    )?;

    assert_supported_mint_extensions(&debt_mint)?;
    assert_supported_mint_extensions(&collateral_mint)?;

    let market_key = *market.info.key;

    create_vault_pda(
        &payer.info,
        debt_vault.info,
        system_program.info,
        &market_key,
        &debt_mint,
        &token_program,
        &token_program_22,
        market_signer.key,
    )?;
    create_vault_pda(
        &payer.info,
        collateral_vault.info,
        system_program.info,
        &market_key,
        &collateral_mint,
        &token_program,
        &token_program_22,
        market_signer.key,
    )?;

    // TWO marginfi-accounts per market — lender-side and
    // borrower-side. Each `marginfi_account_initialize` is signed by
    // the new account's PDA seeds (so marginfi's inner
    // `system::create_account` sees it as signed) plus the
    // `market_signer` PDA seeds (so it signs as the authority on the
    // new account). Payer is the user.
    let market_signer_seeds: &[&[u8]] = &[
        MARKET_SIGNER_SEED,
        market_key.as_ref(),
        &[market_signer_bump],
    ];

    // (1) Lender-side init.
    let lender_seeds: &[&[u8]] = &[
        MARGINFI_LENDER_ACCOUNT_SEED,
        market_key.as_ref(),
        &[lender_marginfi_account_bump],
    ];
    let lender_init_ix = marginfi_mocks::initialize_marginfi_account_ix(
        &marginfi_mocks::InitializeMarginfiAccounts {
            marginfi_group: *marginfi_group.info.key,
            marginfi_account: *lender_marginfi_account.key,
            authority: *market_signer.key,
            fee_payer: *payer.info.key,
            system_program: *system_program.info.key,
        },
    );
    solana_program::program::invoke_signed(
        &lender_init_ix,
        &[
            marginfi_group.info.clone(),
            lender_marginfi_account.clone(),
            market_signer.clone(),
            payer.info.clone(),
            system_program.info.clone(),
            marginfi_program.info.clone(),
        ],
        &[lender_seeds, market_signer_seeds],
    )?;

    // (2) Borrower-side init.
    let borrower_seeds: &[&[u8]] = &[
        MARGINFI_BORROWER_ACCOUNT_SEED,
        market_key.as_ref(),
        &[borrower_marginfi_account_bump],
    ];
    let borrower_init_ix = marginfi_mocks::initialize_marginfi_account_ix(
        &marginfi_mocks::InitializeMarginfiAccounts {
            marginfi_group: *marginfi_group.info.key,
            marginfi_account: *borrower_marginfi_account.key,
            authority: *market_signer.key,
            fee_payer: *payer.info.key,
            system_program: *system_program.info.key,
        },
    );
    solana_program::program::invoke_signed(
        &borrower_init_ix,
        &[
            marginfi_group.info.clone(),
            borrower_marginfi_account.clone(),
            market_signer.clone(),
            payer.info.clone(),
            system_program.info.clone(),
            marginfi_program.info.clone(),
        ],
        &[borrower_seeds, market_signer_seeds],
    )?;

    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let header: &mut MarketFixed = get_mut_helper::<MarketFixed>(market_data, 0_u32);
        *header = MarketFixed::new_empty(&debt_mint, &collateral_mint, &market_key);
        // `payer` is the genesis admin, gates `set_fee_config`.
        header.admin = *payer.info.key;
        header.lender_integration_account = *lender_marginfi_account.key;
        header.lender_integration_account_bump = lender_marginfi_account_bump;
        header.borrower_integration_account = *borrower_marginfi_account.key;
        header.borrower_integration_account_bump = borrower_marginfi_account_bump;
        header.debt_lending_pool = *debt_bank.info.key;
        header.collateral_lending_pool = *collateral_bank.info.key;
        header.marginfi_group = *marginfi_group.info.key;
        header.market_signer = *market_signer.key;
        header.market_signer_bump = market_signer_bump;
    }

    emit_stack(CreateMarketLog {
        market: market_key,
        creator: *payer.info.key,
        debt_mint: *debt_mint.info.key,
        collateral_mint: *collateral_mint.info.key,
    })?;

    expand_market(&payer, &market)?;
    Ok(())
}

pub(crate) fn assert_supported_mint_extensions(mint: &MintAccountInfo) -> ProgramResult {
    if mint.info.owner != &spl_token_2022::id() {
        return Ok(());
    }
    let data = mint.info.try_borrow_data()?;
    let with_ext = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&data)?;
    for ext in with_ext.get_extension_types()? {
        match ext {
            ExtensionType::MetadataPointer => {
                msg!("warning: mint has benign T22 extension {:?}", ext);
            }
            ExtensionType::MintCloseAuthority
            | ExtensionType::TransferFeeConfig
            | ExtensionType::TransferFeeAmount
            | ExtensionType::TransferHook
            | ExtensionType::TransferHookAccount
            | ExtensionType::ConfidentialTransferMint
            | ExtensionType::ConfidentialTransferAccount
            | ExtensionType::PermanentDelegate => {
                // MintCloseAuthority promoted from warn → reject:
                // a T22 mint whose authority can close it mid-protocol
                // would brick deposits/borrows for that mint while
                // existing balances continue to accrue. Without a
                // runtime closed-mint guard on every hot path, the
                // safest move is to refuse such mints up-front.
                msg!("rejecting T22 mint with extension {:?}", ext);
                return Err(YdeltaError::Token2022UnsupportedExtension.into());
            }
            _ => {
                msg!("rejecting T22 mint with unsupported extension {:?}", ext);
                return Err(YdeltaError::Token2022UnsupportedExtension.into());
            }
        }
    }
    Ok(())
}

fn create_vault_pda<'a, 'info>(
    payer: &'a AccountInfo<'info>,
    vault: &'a AccountInfo<'info>,
    system_program: &'a AccountInfo<'info>,
    market_key: &Pubkey,
    mint: &MintAccountInfo<'a, 'info>,
    token_program: &crate::validation::TokenProgram<'a, 'info>,
    token_program_22: &crate::validation::TokenProgram<'a, 'info>,
    vault_authority: &Pubkey,
) -> ProgramResult {
    let (_pda, bump) = crate::validation::get_vault_address(market_key, mint.info.key);
    let mint_owner = mint.info.owner;
    let token_prog_for_mint = if mint_owner == &spl_token_2022::id() {
        token_program_22.info
    } else {
        token_program.info
    };

    let space = if mint_owner == &spl_token_2022::id() {
        ExtensionType::try_calculate_account_len::<spl_token_2022::state::Account>(&[])?
    } else {
        spl_token::state::Account::LEN
    };

    let rent = Rent::get()?;

    let mint_key_bytes = mint.info.key.to_bytes();
    let market_key_bytes = market_key.to_bytes();
    let seeds: Vec<Vec<u8>> = vec![
        b"vault".to_vec(),
        market_key_bytes.to_vec(),
        mint_key_bytes.to_vec(),
        vec![bump],
    ];

    create_account(
        payer,
        vault,
        system_program,
        token_prog_for_mint.key,
        &rent,
        space as u64,
        seeds,
    )?;

    // Vault is owned by `market_signer` (the per-market PDA) so
    // ydelta can sign SPL transfers in/out of it via `invoke_signed`
    // with the market-signer seeds. deposit/withdraw stage atoms here
    // between the user's wallet ATA and the bank's liquidity vault.
    let init_ix = if mint_owner == &spl_token_2022::id() {
        spl_token_2022::instruction::initialize_account3(
            &spl_token_2022::id(),
            vault.key,
            mint.info.key,
            vault_authority,
        )?
    } else {
        spl_token::instruction::initialize_account3(
            &spl_token::id(),
            vault.key,
            mint.info.key,
            vault_authority,
        )?
    };

    invoke(
        &init_ix,
        &[
            vault.clone(),
            mint.info.clone(),
            token_prog_for_mint.clone(),
        ],
    )?;

    Ok::<_, ProgramError>(())
}
