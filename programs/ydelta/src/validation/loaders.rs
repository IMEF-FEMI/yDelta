//! Single source of truth for every instruction's account-list shape.
//! Each `XContext::load(accounts)` parses the raw `&[AccountInfo]` slice
//! in fixed positional order, runs all per-account-shape checks via the
//! validation wrappers ([`Signer`], [`MarginfiBankInfo`],
//! [`YdeltaAccountInfo`], etc), enforces the per-instruction
//! pause / admin gates, and returns a typed context the matching
//! processor consumes. The corresponding `AccountMeta` order lives in
//! [`crate::program::instruction_builders`] — keep the two in lockstep.

use std::cell::Ref;
use std::slice::Iter;

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    program_error::ProgramError,
    pubkey::Pubkey,
    system_program,
};

use hypertree::{HyperTreeReadOperations, NIL};

use crate::program::YdeltaError;
use crate::require;
use crate::state::MarketFixed;
use crate::validation::{
    get_borrower_integration_account_address, get_lender_integration_account_address,
    get_market_signer_address, get_vault_address, EmptyAccount, MarginfiAccountInfo,
    MarginfiBankInfo, MarginfiGroupInfo, MarginfiOracleAis, MarginfiProgram, MintAccountInfo,
    Program, Signer, TokenAccountInfo, TokenProgram, YdeltaAccountInfo,
};

/// Account context for `CreateMarket` (tag 0). Order: `[payer,
/// global_config, market, system_program, debt_mint, collateral_mint,
/// debt_vault, collateral_vault, spl_token, spl_token_2022,
/// marginfi_group, debt_bank, collateral_bank, lender_marginfi_account,
/// borrower_marginfi_account, market_signer, marginfi_program]`. Gated
/// on `signer == GlobalConfig.protocol_admin`.
pub(crate) struct CreateMarketContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub system_program: Program<'a, 'info>,
    pub debt_mint: MintAccountInfo<'a, 'info>,
    pub collateral_mint: MintAccountInfo<'a, 'info>,
    pub debt_vault: EmptyAccount<'a, 'info>,
    pub collateral_vault: EmptyAccount<'a, 'info>,
    pub token_program: TokenProgram<'a, 'info>,
    pub token_program_22: TokenProgram<'a, 'info>,

    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub collateral_bank: MarginfiBankInfo<'a, 'info>,

    pub lender_marginfi_account: &'a AccountInfo<'info>,
    pub lender_marginfi_account_bump: u8,

    pub borrower_marginfi_account: &'a AccountInfo<'info>,
    pub borrower_marginfi_account_bump: u8,

    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
}

impl<'a, 'info> CreateMarketContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let global_config = load_global_config(account_iter)?;
        require!(
            *payer.info.key == global_config.get_fixed()?.protocol_admin,
            YdeltaError::ProtocolAdminRequired,
            "create_market: signer must equal GlobalConfig.protocol_admin"
        )?;
        let market = YdeltaAccountInfo::<MarketFixed>::new_init(next_account_info(account_iter)?)?;
        let system_program = Program::new(next_account_info(account_iter)?, &system_program::id())?;
        let debt_mint = MintAccountInfo::new(next_account_info(account_iter)?)?;
        let collateral_mint = MintAccountInfo::new(next_account_info(account_iter)?)?;
        let debt_vault = EmptyAccount::new(next_account_info(account_iter)?)?;
        let collateral_vault = EmptyAccount::new(next_account_info(account_iter)?)?;

        let (expected_debt_vault, _) = get_vault_address(market.key, debt_mint.info.key);
        let (expected_collateral_vault, _) =
            get_vault_address(market.key, collateral_mint.info.key);

        require!(
            expected_debt_vault == *debt_vault.info.key,
            YdeltaError::IncorrectAccount,
            "Incorrect debt vault account"
        )?;
        require!(
            expected_collateral_vault == *collateral_vault.info.key,
            YdeltaError::IncorrectAccount,
            "Incorrect collateral vault account"
        )?;

        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let token_program_22 = TokenProgram::new(next_account_info(account_iter)?)?;

        let group_ai = next_account_info(account_iter)?;
        let debt_bank_ai = next_account_info(account_iter)?;
        let collateral_bank_ai = next_account_info(account_iter)?;
        let lender_mfi_acct_ai = next_account_info(account_iter)?;
        let borrower_mfi_acct_ai = next_account_info(account_iter)?;
        let market_signer_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;
        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;

        let debt_bank = MarginfiBankInfo::new_with_expected_group(
            debt_bank_ai,
            marginfi_program.info.key,
            group_ai.key,
        )?;
        let collateral_bank = MarginfiBankInfo::new_with_expected_group(
            collateral_bank_ai,
            marginfi_program.info.key,
            group_ai.key,
        )?;

        {
            let dd = debt_bank_ai.try_borrow_data()?;
            let dbank = marginfi_mocks::state::Bank::try_from_account_data(&dd)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            require!(
                dbank.mint == *debt_mint.info.key,
                YdeltaError::IncorrectAccount,
                "debt_bank.mint does not match market.debt_mint"
            )?;
            let cd = collateral_bank_ai.try_borrow_data()?;
            let cbank = marginfi_mocks::state::Bank::try_from_account_data(&cd)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            require!(
                cbank.mint == *collateral_mint.info.key,
                YdeltaError::IncorrectAccount,
                "collateral_bank.mint does not match market.collateral_mint"
            )?;
        }

        let (expected_lender_account, lender_marginfi_account_bump) =
            get_lender_integration_account_address(market.key);
        require!(
            *lender_mfi_acct_ai.key == expected_lender_account,
            YdeltaError::IncorrectAccount,
            "lender_marginfi_account does not match per-market PDA"
        )?;
        let (expected_borrower_account, borrower_marginfi_account_bump) =
            get_borrower_integration_account_address(market.key);
        require!(
            *borrower_mfi_acct_ai.key == expected_borrower_account,
            YdeltaError::IncorrectAccount,
            "borrower_marginfi_account does not match per-market PDA"
        )?;
        let (expected_market_signer, market_signer_bump) = get_market_signer_address(market.key);
        require!(
            *market_signer_ai.key == expected_market_signer,
            YdeltaError::IncorrectAccount,
            "market_signer does not match per-market PDA"
        )?;
        let lender_marginfi_account = lender_mfi_acct_ai;
        let borrower_marginfi_account = borrower_mfi_acct_ai;
        let market_signer = market_signer_ai;

        Ok(Self {
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
        })
    }
}

/// Account context for `ClaimSeat` (tag 1). Order: `[payer,
/// global_config, market, system_program, user_account]`. Lazy-creates
/// the signer's `UserAccountFixed` on first call.
#[allow(dead_code)]
pub(crate) struct ClaimSeatContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub system_program: Program<'a, 'info>,

    pub user_account: YdeltaAccountInfo<'a, 'info, crate::state::user_account::UserAccountFixed>,

    pub user_account_ai: &'a AccountInfo<'info>,
    pub system_program_ai: &'a AccountInfo<'info>,
}

impl<'a, 'info> ClaimSeatContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let system_program_ai = next_account_info(account_iter)?;
        let _ = Program::new(system_program_ai, &system_program::id())?;
        let system_program = Program::new(system_program_ai, &system_program::id())?;

        let user_account_ai = next_account_info(account_iter)?;
        let user_account = crate::validation::user_account::ensure_user_account_for_signer(
            &payer,
            user_account_ai,
            system_program_ai,
        )?;
        Ok(Self {
            payer,
            market,
            system_program,
            user_account,
            user_account_ai,
            system_program_ai,
        })
    }
}

/// Account context for `Deposit` (tag 2). Order: `[payer,
/// global_config, market, trader_token, vault, token_program, mint,
/// marginfi_group, marginfi_account, bank, liquidity_vault,
/// market_signer, marginfi_program, user_account, system_program]`.
/// `is_debt` is inferred from the trader ATA mint matching the market's
/// debt vs collateral side.
pub(crate) struct DepositContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub trader_token: TokenAccountInfo<'a, 'info>,

    pub vault: TokenAccountInfo<'a, 'info>,
    pub token_program: TokenProgram<'a, 'info>,
    pub mint: MintAccountInfo<'a, 'info>,
    pub is_debt: bool,

    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub marginfi_account: MarginfiAccountInfo<'a, 'info>,
    pub bank: MarginfiBankInfo<'a, 'info>,
    pub liquidity_vault: TokenAccountInfo<'a, 'info>,
    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub marginfi_program: MarginfiProgram<'a, 'info>,

    pub user_account_ai: &'a AccountInfo<'info>,
}

impl<'a, 'info> DepositContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;

        let market_fixed: Ref<MarketFixed> = market.get_fixed()?;
        let debt_mint: Pubkey = market_fixed.debt_mint;
        let collateral_mint: Pubkey = market_fixed.collateral_mint;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let collateral_lending_pool: Pubkey = market_fixed.collateral_lending_pool;

        let lender_integration_account: Pubkey = market_fixed.lender_integration_account;
        let borrower_integration_account: Pubkey = market_fixed.borrower_integration_account;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
        let expected_marginfi_group: Pubkey = market_fixed.marginfi_group;
        drop(market_fixed);

        let token_account_info = next_account_info(account_iter)?;
        require!(
            token_account_info.owner == &spl_token::id()
                || token_account_info.owner == &spl_token_2022::id(),
            ProgramError::IllegalOwner,
            "trader token account must be owned by an SPL Token program",
        )?;
        let token_mint_bytes = token_account_info.try_borrow_data()?;
        require!(
            token_mint_bytes.len() >= 32,
            YdeltaError::InvalidDepositAccounts,
            "trader token account data too short to read mint",
        )?;
        let (mint_key, expected_bank, is_debt) =
            if token_mint_bytes[0..32] == debt_mint.to_bytes()[..] {
                (debt_mint, debt_lending_pool, true)
            } else if token_mint_bytes[0..32] == collateral_mint.to_bytes()[..] {
                (collateral_mint, collateral_lending_pool, false)
            } else {
                return Err(YdeltaError::InvalidDepositAccounts.into());
            };
        drop(token_mint_bytes);
        let expected_integration_account = if is_debt {
            lender_integration_account
        } else {
            borrower_integration_account
        };

        let trader_token =
            TokenAccountInfo::new_with_owner(token_account_info, &mint_key, payer.key)?;

        let vault_ai = next_account_info(account_iter)?;
        let (expected_vault, _) = get_vault_address(market.key, &mint_key);
        let vault = TokenAccountInfo::new_with_owner_and_key(
            vault_ai,
            &mint_key,
            &market_signer_pk,
            &expected_vault,
        )?;
        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let mint = MintAccountInfo::new(next_account_info(account_iter)?)?;

        let group_ai = next_account_info(account_iter)?;
        let mfi_acct_ai = next_account_info(account_iter)?;
        let bank_ai = next_account_info(account_iter)?;
        let liquidity_vault_ai = next_account_info(account_iter)?;
        let market_signer_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;

        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        require!(
            *group_ai.key == expected_marginfi_group,
            YdeltaError::IncorrectAccount,
            "marginfi_group does not match MarketFixed.marginfi_group"
        )?;
        let marginfi_account = MarginfiAccountInfo::new_with_expected_authority_and_group(
            mfi_acct_ai,
            marginfi_program.info.key,
            &market_signer_pk,
            &expected_marginfi_group,
        )?;
        let bank = MarginfiBankInfo::new_with_expected_group(
            bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;

        require!(
            *mfi_acct_ai.key == expected_integration_account,
            YdeltaError::IncorrectAccount,
            "marginfi_account does not match the side-relevant per-market PDA \
             (debt deposits → lender_integration_account; collateral → borrower)"
        )?;
        require!(
            *bank_ai.key == expected_bank,
            YdeltaError::IncorrectAccount,
            "bank does not match market's lending pool for this side"
        )?;
        require!(
            *market_signer_ai.key == market_signer_pk,
            YdeltaError::IncorrectAccount,
            "market_signer does not match MarketFixed.market_signer"
        )?;

        let expected_liquidity_vault: Pubkey = {
            let data = bank_ai.try_borrow_data()?;
            let bank_view = marginfi_mocks::state::Bank::try_from_account_data(&data)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            bank_view.liquidity_vault
        };
        require!(
            *liquidity_vault_ai.key == expected_liquidity_vault,
            YdeltaError::IncorrectAccount,
            "liquidity_vault does not match bank.liquidity_vault"
        )?;
        let liquidity_vault = TokenAccountInfo::new(liquidity_vault_ai, &mint_key)?;

        let user_account_ai = next_account_info(account_iter)?;
        let system_program_ai = next_account_info(account_iter)?;
        let _ = Program::new(system_program_ai, &system_program::id())?;
        let _ = crate::validation::user_account::ensure_user_account_for_signer(
            &payer,
            user_account_ai,
            system_program_ai,
        )?;

        Ok(Self {
            payer,
            market,
            trader_token,
            vault,
            token_program,
            mint,
            is_debt,
            marginfi_group,
            marginfi_account,
            bank,
            liquidity_vault,
            market_signer: market_signer_ai,
            market_signer_bump,
            marginfi_program,
            user_account_ai,
        })
    }
}

/// Account context for `Withdraw` (tag 3). Extends the deposit shape
/// with the bank's liquidity-vault authority plus both debt and
/// collateral banks + their oracles (so a debt-side withdraw can
/// re-prove the seat's LTV health before draining).
pub(crate) struct WithdrawContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub trader_token: TokenAccountInfo<'a, 'info>,

    pub vault: TokenAccountInfo<'a, 'info>,
    pub token_program: TokenProgram<'a, 'info>,
    pub mint: MintAccountInfo<'a, 'info>,
    pub is_debt: bool,

    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub marginfi_account: MarginfiAccountInfo<'a, 'info>,

    pub bank: MarginfiBankInfo<'a, 'info>,
    pub liquidity_vault: TokenAccountInfo<'a, 'info>,

    pub bank_liquidity_vault_authority: &'a AccountInfo<'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub collateral_bank: MarginfiBankInfo<'a, 'info>,

    pub debt_oracle_ais: MarginfiOracleAis<'a, 'info>,

    pub collateral_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub marginfi_program: MarginfiProgram<'a, 'info>,

    pub user_account_ai: &'a AccountInfo<'info>,
}

impl<'a, 'info> WithdrawContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;

        let market_fixed: Ref<MarketFixed> = market.get_fixed()?;
        let debt_mint: Pubkey = market_fixed.debt_mint;
        let collateral_mint: Pubkey = market_fixed.collateral_mint;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let collateral_lending_pool: Pubkey = market_fixed.collateral_lending_pool;

        let lender_integration_account: Pubkey = market_fixed.lender_integration_account;
        let borrower_integration_account: Pubkey = market_fixed.borrower_integration_account;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
        let expected_marginfi_group: Pubkey = market_fixed.marginfi_group;
        drop(market_fixed);

        let token_account_info = next_account_info(account_iter)?;
        require!(
            token_account_info.owner == &spl_token::id()
                || token_account_info.owner == &spl_token_2022::id(),
            ProgramError::IllegalOwner,
            "trader token account must be owned by an SPL Token program",
        )?;
        let token_mint_bytes = token_account_info.try_borrow_data()?;
        require!(
            token_mint_bytes.len() >= 32,
            YdeltaError::InvalidWithdrawAccounts,
            "trader token account data too short to read mint",
        )?;
        let (mint_key, _expected_bank, is_debt) =
            if token_mint_bytes[0..32] == debt_mint.to_bytes()[..] {
                (debt_mint, debt_lending_pool, true)
            } else if token_mint_bytes[0..32] == collateral_mint.to_bytes()[..] {
                (collateral_mint, collateral_lending_pool, false)
            } else {
                return Err(YdeltaError::InvalidWithdrawAccounts.into());
            };
        drop(token_mint_bytes);
        let expected_integration_account = if is_debt {
            lender_integration_account
        } else {
            borrower_integration_account
        };

        let trader_token =
            TokenAccountInfo::new_with_owner(token_account_info, &mint_key, payer.key)?;

        let vault_ai = next_account_info(account_iter)?;
        let (expected_vault, _vault_bump) = get_vault_address(market.key, &mint_key);
        let vault = TokenAccountInfo::new_with_owner_and_key(
            vault_ai,
            &mint_key,
            &market_signer_pk,
            &expected_vault,
        )?;
        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let mint = MintAccountInfo::new(next_account_info(account_iter)?)?;

        let group_ai = next_account_info(account_iter)?;
        let mfi_acct_ai = next_account_info(account_iter)?;
        let debt_bank_ai = next_account_info(account_iter)?;
        let collateral_bank_ai = next_account_info(account_iter)?;
        let liquidity_vault_ai = next_account_info(account_iter)?;
        let bank_liquidity_vault_authority_ai = next_account_info(account_iter)?;

        let debt_oracle_ais = MarginfiOracleAis::load(account_iter, debt_bank_ai)?;
        let collateral_oracle_ais = MarginfiOracleAis::load(account_iter, collateral_bank_ai)?;
        let market_signer_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;

        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        require!(
            *group_ai.key == expected_marginfi_group,
            YdeltaError::IncorrectAccount,
            "marginfi_group does not match MarketFixed.marginfi_group"
        )?;
        let marginfi_account = MarginfiAccountInfo::new_with_expected_authority_and_group(
            mfi_acct_ai,
            marginfi_program.info.key,
            &market_signer_pk,
            &expected_marginfi_group,
        )?;
        let debt_bank = MarginfiBankInfo::new_with_expected_group(
            debt_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;
        let collateral_bank = MarginfiBankInfo::new_with_expected_group(
            collateral_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;

        require!(
            *mfi_acct_ai.key == expected_integration_account,
            YdeltaError::IncorrectAccount,
            "marginfi_account does not match the side-relevant integration \
             account: debt withdrawals route to lender_integration_account, \
             collateral withdrawals to borrower_integration_account"
        )?;
        require!(
            *debt_bank_ai.key == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "debt_bank does not match market.debt_lending_pool"
        )?;
        require!(
            *collateral_bank_ai.key == collateral_lending_pool,
            YdeltaError::IncorrectAccount,
            "collateral_bank does not match market.collateral_lending_pool"
        )?;
        require!(
            *market_signer_ai.key == market_signer_pk,
            YdeltaError::IncorrectAccount,
            "market_signer does not match MarketFixed.market_signer"
        )?;

        let bank = if is_debt {
            debt_bank.clone()
        } else {
            collateral_bank.clone()
        };

        let expected_liquidity_vault: Pubkey = {
            let data = bank.info.try_borrow_data()?;
            let bank_view = marginfi_mocks::state::Bank::try_from_account_data(&data)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            bank_view.liquidity_vault
        };
        require!(
            *liquidity_vault_ai.key == expected_liquidity_vault,
            YdeltaError::IncorrectAccount,
            "liquidity_vault does not match bank.liquidity_vault"
        )?;
        let liquidity_vault = TokenAccountInfo::new(liquidity_vault_ai, &mint_key)?;

        let (expected_vault_authority, _) = Pubkey::find_program_address(
            &[b"liquidity_vault_auth", bank.info.key.as_ref()],
            marginfi_program.info.key,
        );
        require!(
            *bank_liquidity_vault_authority_ai.key == expected_vault_authority,
            YdeltaError::IncorrectAccount,
            "bank_liquidity_vault_authority does not match marginfi PDA"
        )?;

        let user_account_ai = next_account_info(account_iter)?;
        let system_program_ai = next_account_info(account_iter)?;
        let _ = Program::new(system_program_ai, &system_program::id())?;
        let _ = crate::validation::user_account::ensure_user_account_for_signer(
            &payer,
            user_account_ai,
            system_program_ai,
        )?;

        Ok(Self {
            payer,
            market,
            trader_token,
            vault,
            token_program,
            mint,
            is_debt,
            marginfi_group,
            marginfi_account,
            bank,
            liquidity_vault,
            bank_liquidity_vault_authority: bank_liquidity_vault_authority_ai,
            debt_bank,
            collateral_bank,
            debt_oracle_ais,
            collateral_oracle_ais,
            market_signer: market_signer_ai,
            market_signer_bump,
            marginfi_program,
            user_account_ai,
        })
    }
}

/// Account context for `PlaceOrder` (tag 4). Carries the borrower's
/// marginfi accounts + both bank oracles for the LTV / fee-floor gates,
/// plus an optional vault-side `GlobalVaultFixed` ref when the order
/// will cross a vault ask. Trailing accounts after `user_account` are
/// the lender_marginfi_account, market_debt_vault, and the optional vault.
pub(crate) struct PlaceOrderContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub _system_program: Program<'a, 'info>,

    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,

    pub borrower_marginfi_account: MarginfiAccountInfo<'a, 'info>,

    pub lender_marginfi_account: MarginfiAccountInfo<'a, 'info>,

    pub market_debt_vault: TokenAccountInfo<'a, 'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub collateral_bank: MarginfiBankInfo<'a, 'info>,

    pub debt_oracle_ais: MarginfiOracleAis<'a, 'info>,

    pub collateral_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub marginfi_program: MarginfiProgram<'a, 'info>,

    pub debt_liquidity_vault: TokenAccountInfo<'a, 'info>,

    pub debt_bank_liquidity_vault_authority: &'a AccountInfo<'info>,

    pub token_program: TokenProgram<'a, 'info>,

    pub user_account_ai: &'a AccountInfo<'info>,

    pub vault: Option<&'a AccountInfo<'info>>,
}

impl<'a, 'info> PlaceOrderContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let _system_program =
            Program::new(next_account_info(account_iter)?, &system_program::id())?;

        let market_fixed: Ref<MarketFixed> = market.get_fixed()?;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let collateral_lending_pool: Pubkey = market_fixed.collateral_lending_pool;

        let borrower_integration_account: Pubkey = market_fixed.borrower_integration_account;
        let lender_integration_account: Pubkey = market_fixed.lender_integration_account;
        let market_debt_vault_pk: Pubkey = market_fixed.debt_vault;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
        let debt_mint_pk: Pubkey = market_fixed.debt_mint;
        let expected_marginfi_group: Pubkey = market_fixed.marginfi_group;
        drop(market_fixed);

        let group_ai = next_account_info(account_iter)?;
        let mfi_acct_ai = next_account_info(account_iter)?;
        let debt_bank_ai = next_account_info(account_iter)?;
        let collateral_bank_ai = next_account_info(account_iter)?;

        let debt_oracle_ais = MarginfiOracleAis::load(account_iter, debt_bank_ai)?;
        let collateral_oracle_ais = MarginfiOracleAis::load(account_iter, collateral_bank_ai)?;
        let market_signer_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;
        let debt_liquidity_vault_ai = next_account_info(account_iter)?;
        let debt_bank_liquidity_vault_authority_ai = next_account_info(account_iter)?;
        let borrower_debt_token_ai = next_account_info(account_iter)?;
        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;

        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        require!(
            *group_ai.key == expected_marginfi_group,
            YdeltaError::IncorrectAccount,
            "marginfi_group does not match MarketFixed.marginfi_group"
        )?;
        let borrower_marginfi_account = MarginfiAccountInfo::new_with_expected_authority_and_group(
            mfi_acct_ai,
            marginfi_program.info.key,
            &market_signer_pk,
            &expected_marginfi_group,
        )?;
        let debt_bank = MarginfiBankInfo::new_with_expected_group(
            debt_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;
        let collateral_bank = MarginfiBankInfo::new_with_expected_group(
            collateral_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;

        require!(
            *mfi_acct_ai.key == borrower_integration_account,
            YdeltaError::IncorrectAccount,
            "marginfi_account does not match borrower_integration_account \
             (place_order routes to borrower-side for P2Pool fallback)"
        )?;
        require!(
            *debt_bank_ai.key == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "debt_bank does not match market.debt_lending_pool"
        )?;
        require!(
            *collateral_bank_ai.key == collateral_lending_pool,
            YdeltaError::IncorrectAccount,
            "collateral_bank does not match market.collateral_lending_pool"
        )?;
        require!(
            *market_signer_ai.key == market_signer_pk,
            YdeltaError::IncorrectAccount,
            "market_signer does not match MarketFixed.market_signer"
        )?;

        let expected_dbg_vault: Pubkey = {
            let data = debt_bank_ai.try_borrow_data()?;
            let bank = marginfi_mocks::state::Bank::try_from_account_data(&data)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            bank.liquidity_vault
        };
        require!(
            *debt_liquidity_vault_ai.key == expected_dbg_vault,
            YdeltaError::IncorrectAccount,
            "debt_liquidity_vault does not match debt_bank.liquidity_vault"
        )?;
        let debt_liquidity_vault = TokenAccountInfo::new(debt_liquidity_vault_ai, &debt_mint_pk)?;

        let (expected_lva, _) = Pubkey::find_program_address(
            &[b"liquidity_vault_auth", debt_bank_ai.key.as_ref()],
            marginfi_program.info.key,
        );
        require!(
            *debt_bank_liquidity_vault_authority_ai.key == expected_lva,
            YdeltaError::IncorrectAccount,
            "debt_bank_liquidity_vault_authority does not match marginfi PDA"
        )?;

        let _ = TokenAccountInfo::new_with_owner(borrower_debt_token_ai, &debt_mint_pk, payer.key)?;

        let user_account_ai = next_account_info(account_iter)?;
        let system_program_ai = next_account_info(account_iter)?;
        let _ = Program::new(system_program_ai, &system_program::id())?;
        let _ = crate::validation::user_account::ensure_user_account_for_signer(
            &payer,
            user_account_ai,
            system_program_ai,
        )?;

        let lender_mfi_acct_ai = next_account_info(account_iter)?;
        let lender_marginfi_account = MarginfiAccountInfo::new_with_expected_authority_and_group(
            lender_mfi_acct_ai,
            marginfi_program.info.key,
            &market_signer_pk,
            &expected_marginfi_group,
        )?;
        require!(
            *lender_mfi_acct_ai.key == lender_integration_account,
            YdeltaError::IncorrectAccount,
            "lender_marginfi_account does not match \
             MarketFixed.lender_integration_account"
        )?;

        let market_debt_vault_ai = next_account_info(account_iter)?;
        require!(
            *market_debt_vault_ai.key == market_debt_vault_pk,
            YdeltaError::IncorrectAccount,
            "market_debt_vault does not match MarketFixed.debt_vault"
        )?;
        let market_debt_vault = TokenAccountInfo::new(market_debt_vault_ai, &debt_mint_pk)?;

        let vault: Option<&'a AccountInfo<'info>> = match account_iter.next() {
            Some(ai) => {
                if ai.data_len() == 0 {
                    None
                } else {
                    let typed_vault =
                        YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(ai)?;
                    let (expected_vault, _) =
                        crate::state::vault::global_vault_pda(&debt_lending_pool);
                    require!(
                        *ai.key == expected_vault,
                        YdeltaError::IncorrectAccount,
                        "vault PDA does not match expected derivation \
                         from market.debt_lending_pool (v1 D1: bank-keyed)"
                    )?;

                    require_vault_not_paused(&typed_vault)?;
                    Some(ai)
                }
            }
            None => None,
        };

        Ok(Self {
            payer,
            market,
            _system_program,
            marginfi_group,
            borrower_marginfi_account,
            lender_marginfi_account,
            market_debt_vault,
            debt_bank,
            collateral_bank,
            debt_oracle_ais,
            collateral_oracle_ais,
            market_signer: market_signer_ai,
            market_signer_bump,
            marginfi_program,
            debt_liquidity_vault,
            debt_bank_liquidity_vault_authority: debt_bank_liquidity_vault_authority_ai,
            token_program,
            user_account_ai,
            vault,
        })
    }
}

/// Account context for `ProcessMatchedLoan` (tag 5). Carries the
/// soon-to-be-allocated loan PDA + bump, the debt bank for the asv
/// snapshot, and (optionally) the per-vault settle accounts when the
/// matched lender is a sub-vault.
pub(crate) struct ProcessMatchedLoanContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,

    pub loan: EmptyAccount<'a, 'info>,

    pub loan_bump: u8,

    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub system_program: Program<'a, 'info>,

    pub queue_node: crate::state::market::MatchedLoan,

    pub queue_node_index: hypertree::DataIndex,

    pub vault_settle: Option<VaultSettleAccounts<'a, 'info>>,
}

#[allow(dead_code)]
/// Per-vault settle accounts pulled when a `ProcessMatchedLoan` cross
/// involves a vault-funded ask. Loaded by [`load_vault_settle_accounts`]
/// after the loan / debt-bank prefix.
pub(crate) struct VaultSettleAccounts<'a, 'info> {
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
    pub global_vault_signer: &'a AccountInfo<'info>,
    pub global_vault_signer_bump: u8,
    pub global_vault_staging: TokenAccountInfo<'a, 'info>,
    pub global_vault_integration_account: MarginfiAccountInfo<'a, 'info>,
    pub market_debt_vault: TokenAccountInfo<'a, 'info>,
    pub market_lender_integration_account: MarginfiAccountInfo<'a, 'info>,
    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub liquidity_vault: TokenAccountInfo<'a, 'info>,
    pub bank_liquidity_vault_authority: &'a AccountInfo<'info>,
    pub bank_oracle: &'a AccountInfo<'info>,
    pub mint: MintAccountInfo<'a, 'info>,
    pub token_program: TokenProgram<'a, 'info>,
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
}

impl<'a, 'info> ProcessMatchedLoanContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>], sequence: u64) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let loan_ai = next_account_info(account_iter)?;
        let debt_bank_ai = next_account_info(account_iter)?;
        let marginfi_program_ai = next_account_info(account_iter)?;
        let system_program = Program::new(next_account_info(account_iter)?, &system_program::id())?;

        require!(
            marginfi_program_ai.key == &marginfi_mocks::ID,
            YdeltaError::IncorrectAccount,
            "marginfi_program key does not match expected program id"
        )?;
        let debt_lending_pool: Pubkey = market.get_fixed()?.debt_lending_pool;
        let expected_marginfi_group: Pubkey = market.get_fixed()?.marginfi_group;
        require!(
            *debt_bank_ai.key == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "debt_bank does not match market.debt_lending_pool"
        )?;

        let debt_bank = MarginfiBankInfo::new_with_expected_group(
            debt_bank_ai,
            marginfi_program_ai.key,
            &expected_marginfi_group,
        )?;

        let (queue_node, queue_node_index) = {
            let root = market.get_fixed()?.matched_loans_root_index;
            let market_data = market.info.try_borrow_data()?;
            let fixed_size = std::mem::size_of::<MarketFixed>();
            let dynamic = &market_data[fixed_size..];
            let tree = crate::state::market::MatchedLoanTreeReadOnly::new(dynamic, root, NIL);
            let mut probe = crate::state::market::MatchedLoan::default();
            probe.sequence = sequence;
            let idx = tree.lookup_index(&probe);
            require!(
                idx != NIL,
                YdeltaError::MatchedLoanNotFound,
                "no MatchedLoan node with sequence {}",
                sequence
            )?;
            let node = *crate::state::market::get_helper_matched_loan(dynamic, idx).get_value();
            (node, idx)
        };

        let (loan, loan_bump) = {
            let (expected_loan, bump) = crate::state::loan::loan_pda(market.key, sequence);
            require!(
                *loan_ai.key == expected_loan,
                YdeltaError::IncorrectAccount,
                "loan account does not match per-(market, sequence) PDA"
            )?;
            (EmptyAccount::new(loan_ai)?, bump)
        };

        let presettled: bool =
            queue_node.flags & crate::state::market::MATCHED_LOAN_FLAG_VAULT_PRESETTLED != 0;
        let vault_settle: Option<VaultSettleAccounts<'a, 'info>> = {
            let trigger_seat_index: hypertree::DataIndex = queue_node.lender_seat_index;
            if trigger_seat_index == NIL || presettled {
                None
            } else {
                let owner_kind: u8 = {
                    let market_data = market.info.try_borrow_data()?;
                    let dynamic = &market_data[std::mem::size_of::<MarketFixed>()..];
                    crate::state::market::get_helper_seat(dynamic, trigger_seat_index)
                        .get_value()
                        .owner_kind
                };
                if owner_kind == crate::state::OWNER_KIND_SUB_VAULT {
                    Some(load_vault_settle_accounts(
                        account_iter,
                        market.key,
                        debt_bank_ai,
                        marginfi_program_ai.key,
                    )?)
                } else {
                    None
                }
            }
        };

        Ok(Self {
            payer,
            market,
            loan,
            loan_bump,
            debt_bank,
            system_program,
            queue_node,
            queue_node_index,
            vault_settle,
        })
    }
}

/// Pull the vault-side accounts a matched-loan promotion needs to move
/// atoms from the vault's `integration_account` into the market's
/// `lender_marginfi_account` and update the lender seat / sub-vault.
pub(crate) fn load_vault_settle_accounts<'a, 'info>(
    iter: &mut Iter<'a, AccountInfo<'info>>,
    market_key: &Pubkey,
    debt_bank_ai: &'a AccountInfo<'info>,
    marginfi_program_id: &Pubkey,
) -> Result<VaultSettleAccounts<'a, 'info>, ProgramError> {
    let vault_ai = next_account_info(iter)?;
    let vault_signer_ai = next_account_info(iter)?;
    let vault_staging_ai = next_account_info(iter)?;
    let vault_integration_ai = next_account_info(iter)?;
    let market_debt_vault_ai = next_account_info(iter)?;
    let market_lender_integration_ai = next_account_info(iter)?;
    let market_signer_ai = next_account_info(iter)?;
    let liquidity_vault_ai = next_account_info(iter)?;
    let bank_liquidity_vault_authority_ai = next_account_info(iter)?;
    let bank_oracle_ai = next_account_info(iter)?;
    let mint_ai = next_account_info(iter)?;
    let token_program_ai = next_account_info(iter)?;
    let marginfi_group_ai = next_account_info(iter)?;
    let marginfi_program_ai = next_account_info(iter)?;

    let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(vault_ai)?;
    require_vault_not_paused(&vault)?;
    let vault_key = *vault_ai.key;

    let (expected_signer, global_vault_signer_bump) =
        crate::state::vault::global_vault_signer_pda(&vault_key);
    require!(
        *vault_signer_ai.key == expected_signer,
        YdeltaError::IncorrectAccount,
        "global_vault_signer PDA mismatch in vault settlement"
    )?;
    let (expected_staging, _) = crate::state::vault::global_vault_staging_pda(&vault_key);
    require!(
        *vault_staging_ai.key == expected_staging,
        YdeltaError::IncorrectAccount,
        "global_vault_staging PDA mismatch in vault settlement"
    )?;
    let (expected_integration, _) =
        crate::state::vault::global_vault_integration_account_pda(&vault_key);
    require!(
        *vault_integration_ai.key == expected_integration,
        YdeltaError::IncorrectAccount,
        "global_vault_integration_account PDA mismatch in vault settlement"
    )?;

    let mint_key = *mint_ai.key;
    let mint = MintAccountInfo::new(mint_ai)?;

    let (expected_global_vault_pda, _) =
        crate::state::vault::global_vault_pda(debt_bank_ai.key);
    require!(
        vault_key == expected_global_vault_pda,
        YdeltaError::IncorrectAccount,
        "vault PDA does not match expected derivation from the debt bank \
         (v1 D1: bank-keyed)"
    )?;

    let expected_marginfi_group: Pubkey = {
        let vfixed = vault.get_fixed()?;
        require!(
            vfixed.mint == mint_key,
            YdeltaError::VaultWrongMint,
            "vault.mint does not match passed mint"
        )?;
        require!(
            vfixed.lending_pool == *debt_bank_ai.key,
            YdeltaError::IncorrectAccount,
            "vault.lending_pool does not match market.debt_bank"
        )?;
        vfixed.integration_pool
    };

    let (expected_market_debt_vault, _) =
        crate::validation::get_vault_address(market_key, &mint_key);
    require!(
        *market_debt_vault_ai.key == expected_market_debt_vault,
        YdeltaError::IncorrectAccount,
        "market_debt_vault PDA mismatch"
    )?;

    let (expected_market_signer, market_signer_bump) =
        crate::validation::get_market_signer_address(market_key);
    require!(
        *market_signer_ai.key == expected_market_signer,
        YdeltaError::IncorrectAccount,
        "market_signer PDA mismatch"
    )?;
    let (expected_lender_integration, _) =
        crate::validation::get_lender_integration_account_address(market_key);
    require!(
        *market_lender_integration_ai.key == expected_lender_integration,
        YdeltaError::IncorrectAccount,
        "market_lender_integration_account PDA mismatch"
    )?;

    {
        let bd = debt_bank_ai.try_borrow_data()?;
        let bank = marginfi_mocks::state::Bank::try_from_account_data(&bd)
            .map_err(|_| YdeltaError::IncorrectAccount)?;
        require!(
            *liquidity_vault_ai.key == bank.liquidity_vault,
            YdeltaError::IncorrectAccount,
            "liquidity_vault does not match debt_bank.liquidity_vault"
        )?;
        let cfg = marginfi_mocks::state::BankConfigView::try_from_account_data(&bd)
            .map_err(|_| YdeltaError::IncorrectAccount)?;
        require!(
            *bank_oracle_ai.key == cfg.primary_oracle(),
            YdeltaError::IncorrectAccount,
            "bank_oracle does not match debt_bank.config.oracle_keys[0]"
        )?;
    }
    let (expected_lva, _) = Pubkey::find_program_address(
        &[b"liquidity_vault_auth", debt_bank_ai.key.as_ref()],
        marginfi_program_id,
    );
    require!(
        *bank_liquidity_vault_authority_ai.key == expected_lva,
        YdeltaError::IncorrectAccount,
        "bank_liquidity_vault_authority does not match marginfi PDA"
    )?;

    let token_program = TokenProgram::new(token_program_ai)?;
    let global_vault_staging = TokenAccountInfo::new_with_owner_and_key(
        vault_staging_ai,
        &mint_key,
        &expected_signer,
        &expected_staging,
    )?;
    let market_debt_vault = TokenAccountInfo::new_with_owner_and_key(
        market_debt_vault_ai,
        &mint_key,
        &expected_market_signer,
        &expected_market_debt_vault,
    )?;
    let liquidity_vault = TokenAccountInfo::new(liquidity_vault_ai, &mint_key)?;

    let global_vault_integration_account =
        MarginfiAccountInfo::new_with_expected_authority_and_group(
            vault_integration_ai,
            marginfi_program_id,
            vault_signer_ai.key,
            &expected_marginfi_group,
        )?;
    let market_lender_integration_account =
        MarginfiAccountInfo::new_with_expected_authority_and_group(
            market_lender_integration_ai,
            marginfi_program_id,
            &expected_market_signer,
            &expected_marginfi_group,
        )?;

    require!(
        marginfi_program_ai.key == marginfi_program_id,
        YdeltaError::IncorrectAccount,
        "vault settle: marginfi_program account mismatch"
    )?;
    let marginfi_program = MarginfiProgram::new(marginfi_program_ai)?;
    let marginfi_group = MarginfiGroupInfo::new(marginfi_group_ai, marginfi_program_id)?;

    require!(
        *marginfi_group_ai.key == expected_marginfi_group,
        YdeltaError::IncorrectAccount,
        "vault settle: marginfi_group does not match vault.integration_pool"
    )?;

    Ok(VaultSettleAccounts {
        vault,
        global_vault_signer: vault_signer_ai,
        global_vault_signer_bump,
        global_vault_staging,
        global_vault_integration_account,
        market_debt_vault,
        market_lender_integration_account,
        market_signer: market_signer_ai,
        market_signer_bump,
        liquidity_vault,
        bank_liquidity_vault_authority: bank_liquidity_vault_authority_ai,
        bank_oracle: bank_oracle_ai,
        mint,
        token_program,
        marginfi_group,
        marginfi_program,
    })
}

#[allow(dead_code)]
/// Account context for `ClaimRepaymentForSubVault` (tag 20). Wires
/// the market's `lender_marginfi_account` source to the vault's
/// `integration_account` destination so the keeper sweep can move
/// `pending_claim_atoms` back into the profile's idle pool.
pub(crate) struct ClaimRepaymentForSubVaultContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub global_vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
    pub global_vault_signer: &'a AccountInfo<'info>,
    pub global_vault_signer_bump: u8,
    pub global_vault_staging: TokenAccountInfo<'a, 'info>,
    pub global_vault_integration_account: MarginfiAccountInfo<'a, 'info>,
    pub market_debt_vault: TokenAccountInfo<'a, 'info>,
    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub lender_marginfi_account: MarginfiAccountInfo<'a, 'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub debt_liquidity_vault: TokenAccountInfo<'a, 'info>,
    pub debt_bank_lva: &'a AccountInfo<'info>,

    pub debt_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub debt_mint: MintAccountInfo<'a, 'info>,
    pub token_program: TokenProgram<'a, 'info>,
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
}

impl<'a, 'info> ClaimRepaymentForSubVaultContext<'a, 'info> {
    /// Loader for the stateless seat→vault sweeper. `sub_vault_id` is
    /// passed via the ix params (not the account list) because the seat
    /// is internal to market.dynamic and looked up by composite key, not
    /// by account address.
    pub fn load(
        accounts: &'a [AccountInfo<'info>],
        _sub_vault_id: u16,
    ) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;

        let market_key = *market.key;
        let market_fixed: Ref<MarketFixed> = market.get_fixed()?;
        let debt_mint_pk: Pubkey = market_fixed.debt_mint;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let lender_integration_pk: Pubkey = market_fixed.lender_integration_account;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
        let expected_marginfi_group: Pubkey = market_fixed.marginfi_group;
        drop(market_fixed);

        let global_vault_ai = next_account_info(account_iter)?;
        let global_vault =
            YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(global_vault_ai)?;

        require_vault_not_paused(&global_vault)?;
        require!(
            global_vault.get_fixed()?.mint == debt_mint_pk,
            YdeltaError::VaultWrongMint,
            "global_vault.mint does not match market.debt_mint"
        )?;
        require!(
            global_vault.get_fixed()?.lending_pool == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "global_vault.lending_pool does not match market.debt_lending_pool"
        )?;

        require!(
            global_vault.get_fixed()?.integration_pool == expected_marginfi_group,
            YdeltaError::IncorrectAccount,
            "global_vault.integration_pool does not match market.marginfi_group"
        )?;

        let global_vault_key = *global_vault.info.key;
        let global_vault_signer_ai = next_account_info(account_iter)?;
        let (expected_gv_signer, global_vault_signer_bump) =
            crate::state::vault::global_vault_signer_pda(&global_vault_key);
        require!(
            *global_vault_signer_ai.key == expected_gv_signer,
            YdeltaError::IncorrectAccount,
            "global_vault_signer PDA mismatch"
        )?;
        let global_vault_staging_ai = next_account_info(account_iter)?;
        let (expected_gv_staging, _) =
            crate::state::vault::global_vault_staging_pda(&global_vault_key);
        require!(
            *global_vault_staging_ai.key == expected_gv_staging,
            YdeltaError::IncorrectAccount,
            "global_vault_staging PDA mismatch"
        )?;
        let global_vault_integration_ai = next_account_info(account_iter)?;
        let (expected_gv_integration, _) =
            crate::state::vault::global_vault_integration_account_pda(&global_vault_key);
        require!(
            *global_vault_integration_ai.key == expected_gv_integration,
            YdeltaError::IncorrectAccount,
            "global_vault_integration_account PDA mismatch"
        )?;

        let market_debt_vault_ai = next_account_info(account_iter)?;
        let (expected_market_debt_vault, _) = get_vault_address(&market_key, &debt_mint_pk);
        require!(
            *market_debt_vault_ai.key == expected_market_debt_vault,
            YdeltaError::IncorrectAccount,
            "market_debt_vault PDA mismatch"
        )?;

        let market_signer_ai = next_account_info(account_iter)?;
        require!(
            *market_signer_ai.key == market_signer_pk,
            YdeltaError::IncorrectAccount,
            "market_signer does not match MarketFixed.market_signer"
        )?;

        let lender_mfi_ai = next_account_info(account_iter)?;
        require!(
            *lender_mfi_ai.key == lender_integration_pk,
            YdeltaError::IncorrectAccount,
            "lender_marginfi_account does not match market.lender_integration_account"
        )?;

        let debt_bank_ai = next_account_info(account_iter)?;
        let debt_liquidity_vault_ai = next_account_info(account_iter)?;
        let debt_bank_lva_ai = next_account_info(account_iter)?;

        let debt_oracle_ais = MarginfiOracleAis::load(account_iter, debt_bank_ai)?;
        let debt_mint_ai = next_account_info(account_iter)?;
        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let group_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;

        require!(
            *debt_bank_ai.key == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "debt_bank does not match market.debt_lending_pool"
        )?;
        require!(
            *debt_mint_ai.key == debt_mint_pk,
            YdeltaError::IncorrectAccount,
            "debt_mint does not match market.debt_mint"
        )?;
        let debt_mint = MintAccountInfo::new(debt_mint_ai)?;

        let debt_bank = MarginfiBankInfo::new_with_expected_group(
            debt_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;

        let lender_marginfi_account = MarginfiAccountInfo::new_with_expected_authority_and_group(
            lender_mfi_ai,
            marginfi_program.info.key,
            &market_signer_pk,
            &expected_marginfi_group,
        )?;
        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        require!(
            *group_ai.key == expected_marginfi_group,
            YdeltaError::IncorrectAccount,
            "marginfi_group does not match MarketFixed.marginfi_group"
        )?;

        {
            let bd = debt_bank_ai.try_borrow_data()?;
            let bank = marginfi_mocks::state::Bank::try_from_account_data(&bd)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            require!(
                *debt_liquidity_vault_ai.key == bank.liquidity_vault,
                YdeltaError::IncorrectAccount,
                "debt_liquidity_vault mismatch"
            )?;
        }
        let debt_liquidity_vault = TokenAccountInfo::new(debt_liquidity_vault_ai, &debt_mint_pk)?;

        let (expected_lva, _) = Pubkey::find_program_address(
            &[b"liquidity_vault_auth", debt_bank_ai.key.as_ref()],
            marginfi_program.info.key,
        );
        require!(
            *debt_bank_lva_ai.key == expected_lva,
            YdeltaError::IncorrectAccount,
            "debt_bank_lva PDA mismatch"
        )?;

        let market_debt_vault = TokenAccountInfo::new_with_owner_and_key(
            market_debt_vault_ai,
            &debt_mint_pk,
            &market_signer_pk,
            &expected_market_debt_vault,
        )?;
        let global_vault_staging = TokenAccountInfo::new_with_owner_and_key(
            global_vault_staging_ai,
            &debt_mint_pk,
            global_vault_signer_ai.key,
            &expected_gv_staging,
        )?;

        let global_vault_integration_account =
            MarginfiAccountInfo::new_with_expected_authority_and_group(
                global_vault_integration_ai,
                marginfi_program.info.key,
                global_vault_signer_ai.key,
                &expected_marginfi_group,
            )?;

        Ok(Self {
            payer,
            market,
            global_vault,
            global_vault_signer: global_vault_signer_ai,
            global_vault_signer_bump,
            global_vault_staging,
            global_vault_integration_account,
            market_debt_vault,
            market_signer: market_signer_ai,
            market_signer_bump,
            lender_marginfi_account,
            debt_bank,
            debt_liquidity_vault,
            debt_bank_lva: debt_bank_lva_ai,
            debt_oracle_ais,
            debt_mint,
            token_program,
            marginfi_group,
            marginfi_program,
        })
    }
}

/// Account context for `Repay` (tag 6). Carries the loan PDA, the
/// borrower's debt ATA, the market's debt-side marginfi account + bank
/// for the repay CPI, and (for P2Pool loans) the borrower marginfi
/// account holding the live liability shares.
pub(crate) struct RepayContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub loan: YdeltaAccountInfo<'a, 'info, crate::state::loan::LoanFixed>,
    pub borrower_token: TokenAccountInfo<'a, 'info>,
    pub vault: TokenAccountInfo<'a, 'info>,
    pub token_program: TokenProgram<'a, 'info>,
    pub debt_mint: MintAccountInfo<'a, 'info>,
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,

    pub marginfi_account: MarginfiAccountInfo<'a, 'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub liquidity_vault: TokenAccountInfo<'a, 'info>,

    #[allow(dead_code)]
    pub collateral_bank: MarginfiBankInfo<'a, 'info>,

    pub borrower_marginfi_account: MarginfiAccountInfo<'a, 'info>,
    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
    pub user_account_ai: &'a AccountInfo<'info>,
    pub cranker_refund: &'a AccountInfo<'info>,
    /// Fixed-loan close-out updates the lender vault's sub-vault
    /// accumulators on full repay. Required for Fixed loans; never read
    /// for P2Pool (the SDK omits the slot for P2Pool repays).
    pub global_vault:
        Option<YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>>,
}

impl<'a, 'info> RepayContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let loan = YdeltaAccountInfo::<crate::state::loan::LoanFixed>::new(next_account_info(
            account_iter,
        )?)?;

        let market_key = *market.key;
        let loan_sequence: u64 = loan.get_fixed()?.matched_loan_sequence;
        let (expected_loan, _bump) = crate::state::loan::loan_pda(&market_key, loan_sequence);
        require!(
            *loan.info.key == expected_loan,
            YdeltaError::IncorrectAccount,
            "loan PDA does not match [b\"loan\", market, sequence={}]",
            loan_sequence
        )?;

        require!(
            loan.get_fixed()?.market == market_key,
            YdeltaError::IncorrectAccount,
            "loan.market does not match passed-in market account"
        )?;

        let market_fixed: Ref<MarketFixed> = market.get_fixed()?;
        let debt_mint_pk: Pubkey = market_fixed.debt_mint;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let collateral_lending_pool: Pubkey = market_fixed.collateral_lending_pool;

        let lender_integration_account: Pubkey = market_fixed.lender_integration_account;
        let borrower_integration_account: Pubkey = market_fixed.borrower_integration_account;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
        let expected_marginfi_group: Pubkey = market_fixed.marginfi_group;
        drop(market_fixed);

        let borrower_token = TokenAccountInfo::new_with_owner(
            next_account_info(account_iter)?,
            &debt_mint_pk,
            payer.key,
        )?;

        let vault_ai = next_account_info(account_iter)?;
        let (expected_vault, _vault_bump) = get_vault_address(&market_key, &debt_mint_pk);
        let vault = TokenAccountInfo::new_with_owner_and_key(
            vault_ai,
            &debt_mint_pk,
            &market_signer_pk,
            &expected_vault,
        )?;

        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let debt_mint = MintAccountInfo::new(next_account_info(account_iter)?)?;

        let group_ai = next_account_info(account_iter)?;
        let mfi_acct_ai = next_account_info(account_iter)?;
        let debt_bank_ai = next_account_info(account_iter)?;
        let liquidity_vault_ai = next_account_info(account_iter)?;
        let collateral_bank_ai = next_account_info(account_iter)?;
        let borrower_mfi_ai = next_account_info(account_iter)?;
        let market_signer_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;

        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        require!(
            *group_ai.key == expected_marginfi_group,
            YdeltaError::IncorrectAccount,
            "marginfi_group does not match MarketFixed.marginfi_group"
        )?;
        let marginfi_account = MarginfiAccountInfo::new_with_expected_authority_and_group(
            mfi_acct_ai,
            marginfi_program.info.key,
            &market_signer_pk,
            &expected_marginfi_group,
        )?;
        let debt_bank = MarginfiBankInfo::new_with_expected_group(
            debt_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;
        let collateral_bank = MarginfiBankInfo::new_with_expected_group(
            collateral_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;

        let borrower_marginfi_account = MarginfiAccountInfo::new_with_expected_authority_and_group(
            borrower_mfi_ai,
            marginfi_program.info.key,
            &market_signer_pk,
            &expected_marginfi_group,
        )?;
        require!(
            *borrower_mfi_ai.key == borrower_integration_account,
            YdeltaError::IncorrectAccount,
            "borrower_marginfi_account does not match market.borrower_integration_account"
        )?;

        require!(
            *mfi_acct_ai.key == lender_integration_account,
            YdeltaError::IncorrectAccount,
            "marginfi_account does not match market.lender_integration_account \
             (Fixed repay routes deposit atoms to the lender side)"
        )?;
        require!(
            *debt_bank_ai.key == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "debt_bank does not match market.debt_lending_pool"
        )?;
        require!(
            *collateral_bank_ai.key == collateral_lending_pool,
            YdeltaError::IncorrectAccount,
            "collateral_bank does not match market.collateral_lending_pool"
        )?;
        require!(
            *market_signer_ai.key == market_signer_pk,
            YdeltaError::IncorrectAccount,
            "market_signer does not match MarketFixed.market_signer"
        )?;

        let expected_liquidity_vault: Pubkey = {
            let data = debt_bank_ai.try_borrow_data()?;
            let bank_view = marginfi_mocks::state::Bank::try_from_account_data(&data)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            bank_view.liquidity_vault
        };
        require!(
            *liquidity_vault_ai.key == expected_liquidity_vault,
            YdeltaError::IncorrectAccount,
            "liquidity_vault does not match debt_bank.liquidity_vault"
        )?;
        let liquidity_vault = TokenAccountInfo::new(liquidity_vault_ai, &debt_mint_pk)?;

        let user_account_ai = next_account_info(account_iter)?;
        let system_program_ai = next_account_info(account_iter)?;
        let _ = Program::new(system_program_ai, &system_program::id())?;
        let _ = crate::validation::user_account::ensure_user_account_for_signer(
            &payer,
            user_account_ai,
            system_program_ai,
        )?;
        let cranker_refund_ai = next_account_info(account_iter)?;
        let loan_created_by: Pubkey = loan.get_fixed()?.created_by;
        require!(
            *cranker_refund_ai.key == loan_created_by,
            YdeltaError::IncorrectAccount,
            "cranker_refund {} does not match loan.created_by {}",
            cranker_refund_ai.key,
            loan_created_by
        )?;

        // Fixed-loan close-out needs the lender's global vault for
        // sub-vault bookkeeping on full repay. P2Pool repays omit
        // this slot — there's no vault lender to update.
        let loan_type = loan.get_fixed()?.loan_type()?;
        let global_vault = if loan_type == crate::state::loan::LoanType::Fixed {
            let gv_ai = next_account_info(account_iter)?;
            let gv =
                YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(gv_ai)?;
            require_vault_not_paused(&gv)?;
            let expected_gv: Pubkey = loan.get_fixed()?.lender_global_vault;
            require!(
                *gv.info.key == expected_gv,
                YdeltaError::IncorrectAccount,
                "global_vault {} does not match loan.lender_global_vault {}",
                gv.info.key,
                expected_gv,
            )?;
            Some(gv)
        } else {
            None
        };

        Ok(Self {
            payer,
            market,
            loan,
            borrower_token,
            vault,
            token_program,
            debt_mint,
            marginfi_group,
            marginfi_account,
            debt_bank,
            liquidity_vault,
            collateral_bank,
            borrower_marginfi_account,
            market_signer: market_signer_ai,
            market_signer_bump,
            marginfi_program,
            user_account_ai,
            cranker_refund: cranker_refund_ai,
            global_vault,
        })
    }
}

/// Account context for `SettleMaturedLoan` (tag 16). Keeper-facing
/// shape: a liquidator ATA pair (debt + collateral), both market
/// integration accounts, and both oracles so the loan can be priced
/// and the seizure transferred under the market signer.
pub(crate) struct SettleMaturedLoanContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub loan: YdeltaAccountInfo<'a, 'info, crate::state::loan::LoanFixed>,
    pub liquidator_debt_token: TokenAccountInfo<'a, 'info>,
    pub liquidator_collateral_token: TokenAccountInfo<'a, 'info>,
    pub market_debt_vault: TokenAccountInfo<'a, 'info>,
    pub market_collateral_vault: TokenAccountInfo<'a, 'info>,
    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub lender_marginfi_account: MarginfiAccountInfo<'a, 'info>,
    pub borrower_marginfi_account: MarginfiAccountInfo<'a, 'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub collateral_bank: MarginfiBankInfo<'a, 'info>,
    pub debt_liquidity_vault: TokenAccountInfo<'a, 'info>,
    pub collateral_liquidity_vault: TokenAccountInfo<'a, 'info>,
    pub collateral_bank_lva: &'a AccountInfo<'info>,

    pub debt_oracle_ais: MarginfiOracleAis<'a, 'info>,

    pub collateral_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub debt_mint: MintAccountInfo<'a, 'info>,
    pub collateral_mint: MintAccountInfo<'a, 'info>,
    pub token_program: TokenProgram<'a, 'info>,
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
    pub cranker_refund: &'a AccountInfo<'info>,
    /// Fixed-loan close-out updates the lender vault's sub-vault on
    /// full liquidate/settle. Required for Fixed loans; SDK omits the
    /// slot for P2Pool.
    pub global_vault:
        Option<YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>>,
}

impl<'a, 'info> SettleMaturedLoanContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let loan = YdeltaAccountInfo::<crate::state::loan::LoanFixed>::new(next_account_info(
            account_iter,
        )?)?;
        let liquidator_debt_token_ai = next_account_info(account_iter)?;
        let liquidator_collateral_token_ai = next_account_info(account_iter)?;
        let market_debt_vault_ai = next_account_info(account_iter)?;
        let market_collateral_vault_ai = next_account_info(account_iter)?;
        let market_signer_ai = next_account_info(account_iter)?;
        let lender_mfi_ai = next_account_info(account_iter)?;
        let borrower_mfi_ai = next_account_info(account_iter)?;
        let debt_bank_ai = next_account_info(account_iter)?;
        let collateral_bank_ai = next_account_info(account_iter)?;
        let debt_liquidity_vault_ai = next_account_info(account_iter)?;
        let collateral_liquidity_vault_ai = next_account_info(account_iter)?;
        let collateral_bank_lva_ai = next_account_info(account_iter)?;

        let debt_oracle_ais = MarginfiOracleAis::load(account_iter, debt_bank_ai)?;
        let collateral_oracle_ais = MarginfiOracleAis::load(account_iter, collateral_bank_ai)?;
        let debt_mint_ai = next_account_info(account_iter)?;
        let collateral_mint_ai = next_account_info(account_iter)?;
        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let group_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;
        let cranker_refund_ai = next_account_info(account_iter)?;

        let market_key = *market.key;
        let loan_sequence: u64 = loan.get_fixed()?.matched_loan_sequence;
        let (expected_loan, _bump) = crate::state::loan::loan_pda(&market_key, loan_sequence);
        require!(
            *loan.info.key == expected_loan,
            YdeltaError::IncorrectAccount,
            "loan PDA mismatch"
        )?;
        require!(
            loan.get_fixed()?.market == market_key,
            YdeltaError::IncorrectAccount,
            "loan.market does not match passed market"
        )?;

        if loan.get_fixed()?.loan_type()? == crate::state::loan::LoanType::Fixed {
            require!(
                loan.get_fixed()?.outstanding_debt_atoms > 0,
                YdeltaError::InvalidArgument,
                "loan already fully repaid (outstanding == 0)"
            )?;
        }

        let market_fixed: Ref<MarketFixed> = market.get_fixed()?;
        let debt_mint_pk: Pubkey = market_fixed.debt_mint;
        let collateral_mint_pk: Pubkey = market_fixed.collateral_mint;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let collateral_lending_pool: Pubkey = market_fixed.collateral_lending_pool;
        let lender_integration_pk: Pubkey = market_fixed.lender_integration_account;
        let borrower_integration_pk: Pubkey = market_fixed.borrower_integration_account;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
        let expected_marginfi_group: Pubkey = market_fixed.marginfi_group;
        drop(market_fixed);

        require!(
            *debt_mint_ai.key == debt_mint_pk,
            YdeltaError::IncorrectAccount,
            "debt_mint mismatch"
        )?;
        require!(
            *collateral_mint_ai.key == collateral_mint_pk,
            YdeltaError::IncorrectAccount,
            "collateral_mint mismatch"
        )?;
        require!(
            *debt_bank_ai.key == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "debt_bank mismatch"
        )?;
        require!(
            *collateral_bank_ai.key == collateral_lending_pool,
            YdeltaError::IncorrectAccount,
            "collateral_bank mismatch"
        )?;
        require!(
            *lender_mfi_ai.key == lender_integration_pk,
            YdeltaError::IncorrectAccount,
            "lender_marginfi_account mismatch"
        )?;
        require!(
            *borrower_mfi_ai.key == borrower_integration_pk,
            YdeltaError::IncorrectAccount,
            "borrower_marginfi_account mismatch"
        )?;
        require!(
            *market_signer_ai.key == market_signer_pk,
            YdeltaError::IncorrectAccount,
            "market_signer mismatch"
        )?;
        let loan_created_by: Pubkey = loan.get_fixed()?.created_by;
        require!(
            *cranker_refund_ai.key == loan_created_by,
            YdeltaError::IncorrectAccount,
            "cranker_refund {} does not match loan.created_by {}",
            cranker_refund_ai.key,
            loan_created_by
        )?;

        let debt_mint = MintAccountInfo::new(debt_mint_ai)?;
        let collateral_mint = MintAccountInfo::new(collateral_mint_ai)?;

        let (expected_debt_vault, _) = get_vault_address(&market_key, &debt_mint_pk);
        let (expected_coll_vault, _) = get_vault_address(&market_key, &collateral_mint_pk);
        require!(
            *market_debt_vault_ai.key == expected_debt_vault,
            YdeltaError::IncorrectAccount,
            "market_debt_vault PDA mismatch"
        )?;
        require!(
            *market_collateral_vault_ai.key == expected_coll_vault,
            YdeltaError::IncorrectAccount,
            "market_collateral_vault PDA mismatch"
        )?;

        let market_debt_vault = TokenAccountInfo::new_with_owner_and_key(
            market_debt_vault_ai,
            &debt_mint_pk,
            &market_signer_pk,
            &expected_debt_vault,
        )?;
        let market_collateral_vault = TokenAccountInfo::new_with_owner_and_key(
            market_collateral_vault_ai,
            &collateral_mint_pk,
            &market_signer_pk,
            &expected_coll_vault,
        )?;
        let liquidator_debt_token = TokenAccountInfo::new_with_owner(
            liquidator_debt_token_ai,
            &debt_mint_pk,
            payer.info.key,
        )?;
        let liquidator_collateral_token = TokenAccountInfo::new_with_owner(
            liquidator_collateral_token_ai,
            &collateral_mint_pk,
            payer.info.key,
        )?;

        let lender_marginfi_account = MarginfiAccountInfo::new_with_expected_authority_and_group(
            lender_mfi_ai,
            marginfi_program.info.key,
            &market_signer_pk,
            &expected_marginfi_group,
        )?;
        let borrower_marginfi_account = MarginfiAccountInfo::new_with_expected_authority_and_group(
            borrower_mfi_ai,
            marginfi_program.info.key,
            &market_signer_pk,
            &expected_marginfi_group,
        )?;
        let debt_bank = MarginfiBankInfo::new_with_expected_group(
            debt_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;
        let collateral_bank = MarginfiBankInfo::new_with_expected_group(
            collateral_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;
        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        require!(
            *group_ai.key == expected_marginfi_group,
            YdeltaError::IncorrectAccount,
            "marginfi_group does not match MarketFixed.marginfi_group"
        )?;

        {
            let dd = debt_bank_ai.try_borrow_data()?;
            let dbank = marginfi_mocks::state::Bank::try_from_account_data(&dd)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            require!(
                *debt_liquidity_vault_ai.key == dbank.liquidity_vault,
                YdeltaError::IncorrectAccount,
                "debt_liquidity_vault mismatch"
            )?;
            let cd = collateral_bank_ai.try_borrow_data()?;
            let cbank = marginfi_mocks::state::Bank::try_from_account_data(&cd)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            require!(
                *collateral_liquidity_vault_ai.key == cbank.liquidity_vault,
                YdeltaError::IncorrectAccount,
                "collateral_liquidity_vault mismatch"
            )?;
        }
        let debt_liquidity_vault = TokenAccountInfo::new(debt_liquidity_vault_ai, &debt_mint_pk)?;
        let collateral_liquidity_vault =
            TokenAccountInfo::new(collateral_liquidity_vault_ai, &collateral_mint_pk)?;

        let (expected_coll_lva, _) = Pubkey::find_program_address(
            &[b"liquidity_vault_auth", collateral_bank_ai.key.as_ref()],
            marginfi_program.info.key,
        );
        require!(
            *collateral_bank_lva_ai.key == expected_coll_lva,
            YdeltaError::IncorrectAccount,
            "collateral_bank_lva PDA mismatch"
        )?;

        // Fixed-loan close-out needs the lender's global vault for
        // sub-vault bookkeeping on full liquidate/settle (mirrors repay).
        // P2Pool ixs omit this slot — their close-out is the marginfi.repay
        // on the borrower's marginfi-account, no vault state to update.
        let loan_type = loan.get_fixed()?.loan_type()?;
        let global_vault = if loan_type == crate::state::loan::LoanType::Fixed {
            let gv_ai = next_account_info(account_iter)?;
            let gv =
                YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(gv_ai)?;
            require_vault_not_paused(&gv)?;
            let expected_gv: Pubkey = loan.get_fixed()?.lender_global_vault;
            require!(
                *gv.info.key == expected_gv,
                YdeltaError::IncorrectAccount,
                "global_vault {} does not match loan.lender_global_vault {}",
                gv.info.key,
                expected_gv,
            )?;
            Some(gv)
        } else {
            None
        };

        Ok(Self {
            payer,
            market,
            loan,
            liquidator_debt_token,
            liquidator_collateral_token,
            market_debt_vault,
            market_collateral_vault,
            market_signer: market_signer_ai,
            market_signer_bump,
            lender_marginfi_account,
            borrower_marginfi_account,
            debt_bank,
            collateral_bank,
            debt_liquidity_vault,
            collateral_liquidity_vault,
            collateral_bank_lva: collateral_bank_lva_ai,
            debt_oracle_ais,
            collateral_oracle_ais,
            debt_mint,
            collateral_mint,
            token_program,
            marginfi_group,
            marginfi_program,
            cranker_refund: cranker_refund_ai,
            global_vault,
        })
    }
}

#[allow(dead_code)]
/// Account context for `ProtocolFeeClaim` (tag 19). Drains the
/// market's accumulated protocol-fee shares from the lender marginfi
/// account into the market admin's debt ATA via marginfi.withdraw.
pub(crate) struct ProtocolFeeClaimContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub admin_debt_token: TokenAccountInfo<'a, 'info>,
    pub market_debt_vault: TokenAccountInfo<'a, 'info>,
    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub lender_marginfi_account: MarginfiAccountInfo<'a, 'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub debt_liquidity_vault: TokenAccountInfo<'a, 'info>,
    pub debt_bank_lva: &'a AccountInfo<'info>,

    pub debt_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub debt_mint: MintAccountInfo<'a, 'info>,
    pub token_program: TokenProgram<'a, 'info>,
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
}

impl<'a, 'info> ProtocolFeeClaimContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let global_config = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let admin_debt_token_ai = next_account_info(account_iter)?;
        let market_debt_vault_ai = next_account_info(account_iter)?;
        let market_signer_ai = next_account_info(account_iter)?;
        let lender_mfi_ai = next_account_info(account_iter)?;
        let debt_bank_ai = next_account_info(account_iter)?;
        let debt_liquidity_vault_ai = next_account_info(account_iter)?;
        let debt_bank_lva_ai = next_account_info(account_iter)?;

        let debt_oracle_ais = MarginfiOracleAis::load(account_iter, debt_bank_ai)?;
        let debt_mint_ai = next_account_info(account_iter)?;
        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let group_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;

        let protocol_admin: Pubkey = global_config.get_fixed()?.protocol_admin;
        require!(
            *payer.info.key == protocol_admin,
            YdeltaError::ProtocolAdminRequired,
            "protocol_fee_claim: signer != GlobalConfig.protocol_admin"
        )?;
        let market_fixed = market.get_fixed()?;
        let debt_mint_pk: Pubkey = market_fixed.debt_mint;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let lender_integration_pk: Pubkey = market_fixed.lender_integration_account;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
        let market_debt_vault_pk: Pubkey = market_fixed.debt_vault;
        let expected_marginfi_group: Pubkey = market_fixed.marginfi_group;
        drop(market_fixed);

        require!(
            *debt_mint_ai.key == debt_mint_pk,
            YdeltaError::IncorrectAccount,
            "debt_mint mismatch"
        )?;
        require!(
            *debt_bank_ai.key == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "debt_bank mismatch"
        )?;
        require!(
            *lender_mfi_ai.key == lender_integration_pk,
            YdeltaError::IncorrectAccount,
            "lender_marginfi_account mismatch"
        )?;
        require!(
            *market_signer_ai.key == market_signer_pk,
            YdeltaError::IncorrectAccount,
            "market_signer mismatch"
        )?;
        require!(
            *market_debt_vault_ai.key == market_debt_vault_pk,
            YdeltaError::IncorrectAccount,
            "market_debt_vault mismatch"
        )?;

        {
            let bd = debt_bank_ai.try_borrow_data()?;
            let bank = marginfi_mocks::state::Bank::try_from_account_data(&bd)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            require!(
                *debt_liquidity_vault_ai.key == bank.liquidity_vault,
                YdeltaError::IncorrectAccount,
                "debt_liquidity_vault mismatch"
            )?;
        }
        let (expected_lva, _) = Pubkey::find_program_address(
            &[b"liquidity_vault_auth", debt_bank_ai.key.as_ref()],
            marginfi_program.info.key,
        );
        require!(
            *debt_bank_lva_ai.key == expected_lva,
            YdeltaError::IncorrectAccount,
            "debt_bank_lva PDA mismatch"
        )?;

        let admin_debt_token =
            TokenAccountInfo::new_with_owner(admin_debt_token_ai, &debt_mint_pk, payer.info.key)?;
        let market_debt_vault = TokenAccountInfo::new(market_debt_vault_ai, &debt_mint_pk)?;
        let debt_liquidity_vault = TokenAccountInfo::new(debt_liquidity_vault_ai, &debt_mint_pk)?;
        let debt_mint = MintAccountInfo::new(debt_mint_ai)?;

        let debt_bank = MarginfiBankInfo::new_with_expected_group(
            debt_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;

        let lender_marginfi_account = MarginfiAccountInfo::new_with_expected_authority_and_group(
            lender_mfi_ai,
            marginfi_program.info.key,
            &market_signer_pk,
            &expected_marginfi_group,
        )?;
        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        require!(
            *group_ai.key == expected_marginfi_group,
            YdeltaError::IncorrectAccount,
            "marginfi_group does not match MarketFixed.marginfi_group"
        )?;

        Ok(Self {
            payer,
            market,
            admin_debt_token,
            market_debt_vault,
            market_signer: market_signer_ai,
            market_signer_bump,
            lender_marginfi_account,
            debt_bank,
            debt_liquidity_vault,
            debt_bank_lva: debt_bank_lva_ai,
            debt_oracle_ais,
            debt_mint,
            token_program,
            marginfi_group,
            marginfi_program,
        })
    }
}

/// Account context for `CreateVault` (tag 8). One-shot per BANK
/// (v1 D1) and permissionless (v1 D3): initializes `GlobalVaultFixed`
/// + its marginfi integration account + the vault signer PDA. First
/// caller becomes `global_vault_admin`.
pub(crate) struct CreateVaultContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,

    pub vault: &'a AccountInfo<'info>,
    pub vault_bump: u8,
    pub mint: MintAccountInfo<'a, 'info>,

    pub global_vault_signer: &'a AccountInfo<'info>,
    pub global_vault_signer_bump: u8,

    pub integration_account: &'a AccountInfo<'info>,
    pub integration_account_bump: u8,

    pub global_vault_staging: &'a AccountInfo<'info>,
    pub global_vault_staging_bump: u8,
    pub token_program: TokenProgram<'a, 'info>,
    pub token_program_22: TokenProgram<'a, 'info>,
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,

    pub lending_pool: MarginfiBankInfo<'a, 'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
    pub system_program: Program<'a, 'info>,
}

impl<'a, 'info> CreateVaultContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        // v1 D3: vault creation is permissionless (one vault per bank,
        // PDA-enforced); the global config is loaded only for the pause
        // gate. The creator becomes the initial global_vault_admin.
        let _ = load_global_config(account_iter)?;
        let vault_ai = next_account_info(account_iter)?;
        let mint = MintAccountInfo::new(next_account_info(account_iter)?)?;
        let vault_signer_ai = next_account_info(account_iter)?;
        let integration_account_ai = next_account_info(account_iter)?;
        let vault_staging_ai = next_account_info(account_iter)?;
        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let token_program_22 = TokenProgram::new(next_account_info(account_iter)?)?;
        let group_ai = next_account_info(account_iter)?;
        let lending_pool_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;
        let system_program = Program::new(next_account_info(account_iter)?, &system_program::id())?;

        let (expected_vault, vault_bump) =
            crate::state::vault::global_vault_pda(lending_pool_ai.key);
        require!(
            *vault_ai.key == expected_vault,
            YdeltaError::IncorrectAccount,
            "vault does not match [b\"vault\", bank] (v1 D1: bank-keyed)"
        )?;
        let (expected_signer, global_vault_signer_bump) =
            crate::state::vault::global_vault_signer_pda(&expected_vault);
        require!(
            *vault_signer_ai.key == expected_signer,
            YdeltaError::IncorrectAccount,
            "global_vault_signer does not match [b\"global_vault_signer\", vault]"
        )?;
        let (expected_integration, integration_account_bump) =
            crate::state::vault::global_vault_integration_account_pda(&expected_vault);
        require!(
            *integration_account_ai.key == expected_integration,
            YdeltaError::IncorrectAccount,
            "integration_account does not match [b\"vault_integration\", vault]"
        )?;
        let (expected_staging, global_vault_staging_bump) =
            crate::state::vault::global_vault_staging_pda(&expected_vault);
        require!(
            *vault_staging_ai.key == expected_staging,
            YdeltaError::IncorrectAccount,
            "global_vault_staging does not match [b\"global_vault_staging\", vault]"
        )?;

        require!(
            vault_ai.data_is_empty(),
            YdeltaError::IncorrectAccount,
            "vault PDA already exists; one GlobalVault per bank"
        )?;
        require!(
            integration_account_ai.data_is_empty(),
            YdeltaError::IncorrectAccount,
            "integration_account already exists; one per vault"
        )?;
        require!(
            vault_staging_ai.data_is_empty(),
            YdeltaError::IncorrectAccount,
            "global_vault_staging already exists; one per vault"
        )?;

        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;

        let lending_pool = MarginfiBankInfo::new_with_expected_group(
            lending_pool_ai,
            marginfi_program.info.key,
            group_ai.key,
        )?;

        {
            let bank_data = lending_pool_ai.try_borrow_data()?;
            let bank = marginfi_mocks::state::Bank::try_from_account_data(&bank_data)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            require!(
                bank.mint == *mint.info.key,
                YdeltaError::VaultWrongMint,
                "lending_pool.mint does not match vault.mint"
            )?;
        }

        Ok(Self {
            payer,
            vault: vault_ai,
            vault_bump,
            mint,
            global_vault_signer: vault_signer_ai,
            global_vault_signer_bump,
            integration_account: integration_account_ai,
            integration_account_bump,
            global_vault_staging: vault_staging_ai,
            global_vault_staging_bump,
            token_program,
            token_program_22,
            marginfi_group,
            lending_pool,
            marginfi_program,
            system_program,
        })
    }
}

/// Account context for `GlobalVaultDeposit` (tag 10). Routes the
/// depositor's ATA into the vault's marginfi integration account and
/// mints fp48 profile shares against the selected `sub_vault_id`.
pub(crate) struct GlobalVaultDepositContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
    pub mint: MintAccountInfo<'a, 'info>,

    pub global_vault_signer: &'a AccountInfo<'info>,
    pub global_vault_signer_bump: u8,
    pub global_vault_staging: TokenAccountInfo<'a, 'info>,
    pub depositor_token: TokenAccountInfo<'a, 'info>,
    pub token_program: TokenProgram<'a, 'info>,
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub integration_account: MarginfiAccountInfo<'a, 'info>,
    pub lending_pool: MarginfiBankInfo<'a, 'info>,
    pub liquidity_vault: TokenAccountInfo<'a, 'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
    pub user_account_ai: &'a AccountInfo<'info>,
}

impl<'a, 'info> GlobalVaultDepositContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;

        require_vault_not_paused(&vault)?;
        let mint = MintAccountInfo::new(next_account_info(account_iter)?)?;
        let vault_signer_ai = next_account_info(account_iter)?;
        let vault_staging_ai = next_account_info(account_iter)?;
        let depositor_token_ai = next_account_info(account_iter)?;
        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let group_ai = next_account_info(account_iter)?;
        let integration_ai = next_account_info(account_iter)?;
        let lending_pool_ai = next_account_info(account_iter)?;
        let liquidity_vault_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;
        let user_account_ai = next_account_info(account_iter)?;
        let system_program_ai = next_account_info(account_iter)?;
        let _ = Program::new(system_program_ai, &system_program::id())?;

        let vault_key = *vault.info.key;
        let (expected_signer, global_vault_signer_bump) =
            crate::state::vault::global_vault_signer_pda(&vault_key);
        require!(
            *vault_signer_ai.key == expected_signer,
            YdeltaError::IncorrectAccount,
            "global_vault_signer PDA mismatch"
        )?;
        let (expected_staging, _) = crate::state::vault::global_vault_staging_pda(&vault_key);
        require!(
            *vault_staging_ai.key == expected_staging,
            YdeltaError::IncorrectAccount,
            "global_vault_staging PDA mismatch"
        )?;
        let (expected_integration, _) =
            crate::state::vault::global_vault_integration_account_pda(&vault_key);
        require!(
            *integration_ai.key == expected_integration,
            YdeltaError::IncorrectAccount,
            "integration_account PDA mismatch"
        )?;

        let mint_key = *mint.info.key;
        let vault_fixed = vault.get_fixed()?;
        require!(
            vault_fixed.mint == mint_key,
            YdeltaError::VaultWrongMint,
            "passed mint does not match vault.mint"
        )?;
        require!(
            vault_fixed.lending_pool == *lending_pool_ai.key,
            YdeltaError::IncorrectAccount,
            "lending_pool does not match vault.lending_pool"
        )?;
        let expected_marginfi_group: Pubkey = vault_fixed.integration_pool;
        drop(vault_fixed);

        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        require!(
            *group_ai.key == expected_marginfi_group,
            YdeltaError::IncorrectAccount,
            "marginfi_group does not match GlobalVaultFixed.integration_pool"
        )?;
        let integration_account = MarginfiAccountInfo::new_with_expected_authority_and_group(
            integration_ai,
            marginfi_program.info.key,
            &expected_signer,
            &expected_marginfi_group,
        )?;
        let lending_pool = MarginfiBankInfo::new_with_expected_group(
            lending_pool_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;
        let global_vault_staging = TokenAccountInfo::new_with_owner_and_key(
            vault_staging_ai,
            &mint_key,
            vault_signer_ai.key,
            &expected_staging,
        )?;
        let depositor_token =
            TokenAccountInfo::new_with_owner(depositor_token_ai, &mint_key, payer.info.key)?;

        let expected_liquidity_vault: Pubkey = {
            let bd = lending_pool_ai.try_borrow_data()?;
            let bank = marginfi_mocks::state::Bank::try_from_account_data(&bd)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            bank.liquidity_vault
        };
        require!(
            *liquidity_vault_ai.key == expected_liquidity_vault,
            YdeltaError::IncorrectAccount,
            "liquidity_vault does not match lending_pool.liquidity_vault"
        )?;
        let liquidity_vault = TokenAccountInfo::new(liquidity_vault_ai, &mint_key)?;

        crate::validation::user_account::ensure_user_account_for_signer(
            &payer,
            user_account_ai,
            system_program_ai,
        )?;

        Ok(Self {
            payer,
            vault,
            mint,
            global_vault_signer: vault_signer_ai,
            global_vault_signer_bump,
            global_vault_staging,
            depositor_token,
            token_program,
            marginfi_group,
            integration_account,
            lending_pool,
            liquidity_vault,
            marginfi_program,
            user_account_ai,
        })
    }
}

/// Account context for `GlobalVaultWithdraw` (tag 11). Burns profile
/// shares, withdraws atoms from the vault's marginfi account to the
/// depositor's ATA. Rejects burns that would drop idle below the per-
/// profile gate.
pub(crate) struct GlobalVaultWithdrawContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
    pub mint: MintAccountInfo<'a, 'info>,
    pub global_vault_signer: &'a AccountInfo<'info>,
    pub global_vault_signer_bump: u8,
    pub global_vault_staging: TokenAccountInfo<'a, 'info>,
    pub depositor_token: TokenAccountInfo<'a, 'info>,
    pub token_program: TokenProgram<'a, 'info>,
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub integration_account: MarginfiAccountInfo<'a, 'info>,
    pub lending_pool: MarginfiBankInfo<'a, 'info>,
    pub lending_pool_oracle: &'a AccountInfo<'info>,
    pub liquidity_vault: TokenAccountInfo<'a, 'info>,
    pub bank_liquidity_vault_authority: &'a AccountInfo<'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
    pub user_account_ai: &'a AccountInfo<'info>,
}

impl<'a, 'info> GlobalVaultWithdrawContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;

        require_vault_not_paused(&vault)?;
        let mint = MintAccountInfo::new(next_account_info(account_iter)?)?;
        let vault_signer_ai = next_account_info(account_iter)?;
        let vault_staging_ai = next_account_info(account_iter)?;
        let depositor_token_ai = next_account_info(account_iter)?;
        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let group_ai = next_account_info(account_iter)?;
        let integration_ai = next_account_info(account_iter)?;
        let lending_pool_ai = next_account_info(account_iter)?;
        let oracle_ai = next_account_info(account_iter)?;
        let liquidity_vault_ai = next_account_info(account_iter)?;
        let bank_liquidity_vault_authority_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;
        let user_account_ai = next_account_info(account_iter)?;
        let system_program_ai = next_account_info(account_iter)?;
        let _ = Program::new(system_program_ai, &system_program::id())?;

        let vault_key = *vault.info.key;
        let (expected_signer, global_vault_signer_bump) =
            crate::state::vault::global_vault_signer_pda(&vault_key);
        require!(
            *vault_signer_ai.key == expected_signer,
            YdeltaError::IncorrectAccount,
            "global_vault_signer PDA mismatch"
        )?;
        let (expected_staging, _) = crate::state::vault::global_vault_staging_pda(&vault_key);
        require!(
            *vault_staging_ai.key == expected_staging,
            YdeltaError::IncorrectAccount,
            "global_vault_staging PDA mismatch"
        )?;
        let (expected_integration, _) =
            crate::state::vault::global_vault_integration_account_pda(&vault_key);
        require!(
            *integration_ai.key == expected_integration,
            YdeltaError::IncorrectAccount,
            "integration_account PDA mismatch"
        )?;

        let mint_key = *mint.info.key;
        let vault_fixed = vault.get_fixed()?;
        require!(
            vault_fixed.mint == mint_key,
            YdeltaError::VaultWrongMint,
            "passed mint does not match vault.mint"
        )?;
        require!(
            vault_fixed.lending_pool == *lending_pool_ai.key,
            YdeltaError::IncorrectAccount,
            "lending_pool does not match vault.lending_pool"
        )?;
        let expected_marginfi_group: Pubkey = vault_fixed.integration_pool;
        drop(vault_fixed);

        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        require!(
            *group_ai.key == expected_marginfi_group,
            YdeltaError::IncorrectAccount,
            "marginfi_group does not match GlobalVaultFixed.integration_pool"
        )?;
        let integration_account = MarginfiAccountInfo::new_with_expected_authority_and_group(
            integration_ai,
            marginfi_program.info.key,
            &expected_signer,
            &expected_marginfi_group,
        )?;
        let lending_pool = MarginfiBankInfo::new_with_expected_group(
            lending_pool_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;
        let global_vault_staging = TokenAccountInfo::new_with_owner_and_key(
            vault_staging_ai,
            &mint_key,
            vault_signer_ai.key,
            &expected_staging,
        )?;
        let depositor_token =
            TokenAccountInfo::new_with_owner(depositor_token_ai, &mint_key, payer.info.key)?;

        {
            let bd = lending_pool_ai.try_borrow_data()?;
            let bank = marginfi_mocks::state::Bank::try_from_account_data(&bd)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            require!(
                *liquidity_vault_ai.key == bank.liquidity_vault,
                YdeltaError::IncorrectAccount,
                "liquidity_vault does not match lending_pool.liquidity_vault"
            )?;
            let cfg = marginfi_mocks::state::BankConfigView::try_from_account_data(&bd)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            require!(
                *oracle_ai.key == cfg.primary_oracle(),
                YdeltaError::IncorrectAccount,
                "oracle does not match lending_pool.config.oracle_keys[0]"
            )?;
        }
        let liquidity_vault = TokenAccountInfo::new(liquidity_vault_ai, &mint_key)?;

        let (expected_lva, _) = Pubkey::find_program_address(
            &[b"liquidity_vault_auth", lending_pool_ai.key.as_ref()],
            marginfi_program.info.key,
        );
        require!(
            *bank_liquidity_vault_authority_ai.key == expected_lva,
            YdeltaError::IncorrectAccount,
            "bank_liquidity_vault_authority does not match marginfi PDA"
        )?;

        crate::validation::user_account::ensure_user_account_for_signer(
            &payer,
            user_account_ai,
            system_program_ai,
        )?;

        Ok(Self {
            payer,
            vault,
            mint,
            global_vault_signer: vault_signer_ai,
            global_vault_signer_bump,
            global_vault_staging,
            depositor_token,
            token_program,
            marginfi_group,
            integration_account,
            lending_pool,
            lending_pool_oracle: oracle_ai,
            liquidity_vault,
            bank_liquidity_vault_authority: bank_liquidity_vault_authority_ai,
            marginfi_program,
            user_account_ai,
        })
    }
}

#[allow(dead_code)]
/// Account context for `ClaimCuratorFee` (tag 15). Curator-gated;
/// transfers the profile's accumulated curator fee atoms from the
/// vault's marginfi account to the curator's ATA.
pub(crate) struct ClaimCuratorFeeContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
    pub global_vault_signer: &'a AccountInfo<'info>,
    pub global_vault_signer_bump: u8,
    pub global_vault_staging: TokenAccountInfo<'a, 'info>,
    pub global_vault_integration_account: MarginfiAccountInfo<'a, 'info>,
    pub curator_token: TokenAccountInfo<'a, 'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub liquidity_vault: TokenAccountInfo<'a, 'info>,
    pub bank_liquidity_vault_authority: &'a AccountInfo<'info>,
    pub bank_oracle: &'a AccountInfo<'info>,
    pub mint: MintAccountInfo<'a, 'info>,
    pub token_program: TokenProgram<'a, 'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
}

impl<'a, 'info> ClaimCuratorFeeContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;

        require_vault_not_paused(&vault)?;
        let vault_signer_ai = next_account_info(account_iter)?;
        let vault_staging_ai = next_account_info(account_iter)?;
        let vault_integration_ai = next_account_info(account_iter)?;
        let curator_token_ai = next_account_info(account_iter)?;
        let debt_bank_ai = next_account_info(account_iter)?;
        let liquidity_vault_ai = next_account_info(account_iter)?;
        let bank_liquidity_vault_authority_ai = next_account_info(account_iter)?;
        let bank_oracle_ai = next_account_info(account_iter)?;
        let mint_ai = next_account_info(account_iter)?;
        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;
        let marginfi_group_ai = next_account_info(account_iter)?;
        let marginfi_group = MarginfiGroupInfo::new(marginfi_group_ai, marginfi_program.info.key)?;

        let vault_key = *vault.info.key;
        let (expected_signer, global_vault_signer_bump) =
            crate::state::vault::global_vault_signer_pda(&vault_key);
        require!(
            *vault_signer_ai.key == expected_signer,
            YdeltaError::IncorrectAccount,
            "global_vault_signer PDA mismatch"
        )?;
        let (expected_staging, _) = crate::state::vault::global_vault_staging_pda(&vault_key);
        require!(
            *vault_staging_ai.key == expected_staging,
            YdeltaError::IncorrectAccount,
            "global_vault_staging PDA mismatch"
        )?;
        let (expected_integration, _) =
            crate::state::vault::global_vault_integration_account_pda(&vault_key);
        require!(
            *vault_integration_ai.key == expected_integration,
            YdeltaError::IncorrectAccount,
            "global_vault_integration_account PDA mismatch"
        )?;

        let mint_key = *mint_ai.key;
        let mint = MintAccountInfo::new(mint_ai)?;
        require!(
            vault.get_fixed()?.mint == mint_key,
            YdeltaError::VaultWrongMint,
            "vault.mint does not match passed mint"
        )?;
        require!(
            vault.get_fixed()?.lending_pool == *debt_bank_ai.key,
            YdeltaError::IncorrectAccount,
            "debt_bank does not match vault.lending_pool"
        )?;

        let expected_marginfi_group: Pubkey = vault.get_fixed()?.integration_pool;
        require!(
            *marginfi_group_ai.key == expected_marginfi_group,
            YdeltaError::IncorrectAccount,
            "marginfi_group does not match GlobalVaultFixed.integration_pool"
        )?;
        let debt_bank = MarginfiBankInfo::new_with_expected_group(
            debt_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;

        {
            let bd = debt_bank_ai.try_borrow_data()?;
            let bank = marginfi_mocks::state::Bank::try_from_account_data(&bd)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            require!(
                *liquidity_vault_ai.key == bank.liquidity_vault,
                YdeltaError::IncorrectAccount,
                "liquidity_vault mismatch"
            )?;
            let cfg = marginfi_mocks::state::BankConfigView::try_from_account_data(&bd)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            require!(
                *bank_oracle_ai.key == cfg.primary_oracle(),
                YdeltaError::IncorrectAccount,
                "bank_oracle mismatch"
            )?;
        }
        let (expected_lva, _) = Pubkey::find_program_address(
            &[b"liquidity_vault_auth", debt_bank_ai.key.as_ref()],
            marginfi_program.info.key,
        );
        require!(
            *bank_liquidity_vault_authority_ai.key == expected_lva,
            YdeltaError::IncorrectAccount,
            "bank_liquidity_vault_authority mismatch"
        )?;

        let global_vault_staging = TokenAccountInfo::new_with_owner_and_key(
            vault_staging_ai,
            &mint_key,
            vault_signer_ai.key,
            &expected_staging,
        )?;
        let curator_token =
            TokenAccountInfo::new_with_owner(curator_token_ai, &mint_key, payer.info.key)?;
        let liquidity_vault = TokenAccountInfo::new(liquidity_vault_ai, &mint_key)?;

        let global_vault_integration_account =
            MarginfiAccountInfo::new_with_expected_authority_and_group(
                vault_integration_ai,
                marginfi_program.info.key,
                &expected_signer,
                &expected_marginfi_group,
            )?;

        Ok(Self {
            payer,
            vault,
            global_vault_signer: vault_signer_ai,
            global_vault_signer_bump,
            global_vault_staging,
            global_vault_integration_account,
            curator_token,
            debt_bank,
            liquidity_vault,
            bank_liquidity_vault_authority: bank_liquidity_vault_authority_ai,
            bank_oracle: bank_oracle_ai,
            mint,
            token_program,
            marginfi_program,
            marginfi_group,
        })
    }
}

/// Account context for `CancelOrderForSubVault` (tag 13).
/// Curator-gated; clears both the resting order on the market book
/// and the vault-side `SubVaultOrderRef`.
pub(crate) struct CancelOrderForSubVaultContext<'a, 'info> {
    pub fee_payer: Signer<'a, 'info>,
    pub curator: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub _system_program: Program<'a, 'info>,
}

impl<'a, 'info> CancelOrderForSubVaultContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let fee_payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let curator = Signer::new(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;

        require_vault_not_paused(&vault)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let _system_program =
            Program::new(next_account_info(account_iter)?, &system_program::id())?;

        require_vault_mint_matches_market(&vault, &market)?;

        Ok(Self {
            fee_payer,
            curator,
            vault,
            market,
            _system_program,
        })
    }
}

/// Permissionless sub-vault mutation context: payer + global-config
/// pause gate + vault (pause-gated) + system program. Used by
/// `CreatePrivateSubVault` (anyone may open a Private sub-vault, v1 D2)
/// and `UpdateSubVault` (the curator gate lives in the processor, which
/// must look the sub-vault up anyway, v1 D15).
pub(crate) struct SubVaultMutationContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
    pub _system_program: Program<'a, 'info>,
}

impl<'a, 'info> SubVaultMutationContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;

        require_vault_not_paused(&vault)?;
        let _system_program =
            Program::new(next_account_info(account_iter)?, &system_program::id())?;

        Ok(Self {
            payer,
            vault,
            _system_program,
        })
    }
}

/// Account context for `CreatePoolSubVault` (tag 9). Protocol-admin
/// gated (v1 D3: the admin creates the Pool and assigns its curator);
/// appends a `SubVault` to the vault tree with a monotonic sub_vault_id.
pub(crate) struct CreatePoolSubVaultContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
    pub _system_program: Program<'a, 'info>,
}

impl<'a, 'info> CreatePoolSubVaultContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let global_config = load_global_config(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;

        require_vault_not_paused(&vault)?;
        let _system_program =
            Program::new(next_account_info(account_iter)?, &system_program::id())?;

        let protocol_admin: Pubkey = global_config.get_fixed()?.protocol_admin;
        require!(
            *payer.info.key == protocol_admin,
            YdeltaError::ProtocolAdminRequired,
            "create_pool_sub_vault: signer ({}) is not GlobalConfig.protocol_admin ({})",
            payer.info.key,
            protocol_admin
        )?;

        Ok(Self {
            payer,
            vault,
            _system_program,
        })
    }
}

/// Account context for `RemoveSubVault` (tag 37). Vault-admin
/// gated; removes a sunset, fully drained profile from the vault tree.
pub(crate) struct RemoveSubVaultContext<'a, 'info> {
    pub _payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
}

impl<'a, 'info> RemoveSubVaultContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        require_vault_not_paused(&vault)?;

        let global_vault_admin: Pubkey = vault.get_fixed()?.global_vault_admin;
        require!(
            *payer.info.key == global_vault_admin,
            YdeltaError::VaultAdminRequired,
            "remove_sub_vault: signer ({}) is not global_vault_admin ({})",
            payer.info.key,
            global_vault_admin
        )?;

        Ok(Self {
            _payer: payer,
            vault,
        })
    }
}

/// Account context for `SunsetSubVault` (tag 38). Vault-admin
/// gated; flips `profile.is_sunset = 1`.
pub(crate) struct SunsetSubVaultContext<'a, 'info> {
    pub _payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
}

impl<'a, 'info> SunsetSubVaultContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config_no_pause(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        let global_vault_admin: Pubkey = vault.get_fixed()?.global_vault_admin;
        require!(
            *payer.info.key == global_vault_admin,
            YdeltaError::VaultAdminRequired,
            "sunset_sub_vault: signer ({}) is not global_vault_admin ({})",
            payer.info.key,
            global_vault_admin
        )?;
        Ok(Self {
            _payer: payer,
            vault,
        })
    }
}

/// Account context for `ResumeSubVault` (tag 39). Vault-admin
/// gated; flips `profile.is_sunset = 0`.
pub(crate) struct ResumeSubVaultContext<'a, 'info> {
    pub _payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
}

impl<'a, 'info> ResumeSubVaultContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config_no_pause(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        let global_vault_admin: Pubkey = vault.get_fixed()?.global_vault_admin;
        require!(
            *payer.info.key == global_vault_admin,
            YdeltaError::VaultAdminRequired,
            "resume_sub_vault: signer ({}) is not global_vault_admin ({})",
            payer.info.key,
            global_vault_admin
        )?;
        Ok(Self {
            _payer: payer,
            vault,
        })
    }
}

/// Account context for `AdminCancelSubVaultOrder` (tag 40).
/// Vault-admin gated; force-cancels a sunset profile's resting ask
/// (curator-only on non-sunset profiles).
pub(crate) struct AdminCancelSubVaultOrderContext<'a, 'info> {
    pub _payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
}

impl<'a, 'info> AdminCancelSubVaultOrderContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config_no_pause(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        let global_vault_admin: Pubkey = vault.get_fixed()?.global_vault_admin;
        require!(
            *payer.info.key == global_vault_admin,
            YdeltaError::VaultAdminRequired,
            "admin_cancel_sub_vault_order: signer ({}) is not global_vault_admin ({})",
            payer.info.key,
            global_vault_admin
        )?;
        require_vault_mint_matches_market(&vault, &market)?;
        Ok(Self {
            _payer: payer,
            vault,
            market,
        })
    }
}

/// Account context for `TransferMarketAdmin` (tag 21). Two-step
/// handoff: sets `pending_admin`; gated on `signer == current admin`.
pub(crate) struct TransferMarketAdminContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
}

impl<'a, 'info> TransferMarketAdminContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;

        let _ = load_global_config_no_pause(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        Ok(Self { payer, market })
    }
}

/// Account context for `AcceptMarketAdmin` (tag 22). Finalizes the
/// handoff; gated on `signer == pending_admin`.
pub(crate) struct AcceptMarketAdminContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
}

impl<'a, 'info> AcceptMarketAdminContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;

        let _ = load_global_config_no_pause(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        Ok(Self { payer, market })
    }
}

/// Account context for `TransferGlobalVaultAdmin` (tag 23). Two-step
/// vault-admin handoff.
pub(crate) struct TransferGlobalVaultAdminContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
}

impl<'a, 'info> TransferGlobalVaultAdminContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;

        let _ = load_global_config_no_pause(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        Ok(Self { payer, vault })
    }
}

/// Account context for `AcceptGlobalVaultAdmin` (tag 24).
pub(crate) struct AcceptGlobalVaultAdminContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
}

impl<'a, 'info> AcceptGlobalVaultAdminContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;

        let _ = load_global_config_no_pause(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        Ok(Self { payer, vault })
    }
}

/// Account context for `TransferCurator` (tag 25). Per-profile
/// two-step handoff; gated on `signer == profile.curator`.
pub(crate) struct TransferCuratorContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
}

impl<'a, 'info> TransferCuratorContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;

        let _ = load_global_config_no_pause(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        Ok(Self { payer, vault })
    }
}

/// Account context for `AcceptCurator` (tag 26).
pub(crate) struct AcceptCuratorContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
}

impl<'a, 'info> AcceptCuratorContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;

        let _ = load_global_config_no_pause(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        Ok(Self { payer, vault })
    }
}

/// Account context for `SetMarketPause` (tag 27). Market-admin gated;
/// flips `MarketFixed.is_paused`.
pub(crate) struct SetMarketPauseContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
}

impl<'a, 'info> SetMarketPauseContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;

        let _ = load_global_config_no_pause(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        Ok(Self { payer, market })
    }
}

/// Account context for `SetVaultPause` (tag 36). Vault-admin gated;
/// flips `GlobalVaultFixed.is_paused`.
pub(crate) struct SetVaultPauseContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
}

impl<'a, 'info> SetVaultPauseContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;

        let _ = load_global_config_no_pause(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        Ok(Self { payer, vault })
    }
}

/// Reject state-mutating market ixs when `MarketFixed.is_paused == 1`.
pub(crate) fn require_market_not_paused(
    market: &YdeltaAccountInfo<'_, '_, MarketFixed>,
) -> Result<(), ProgramError> {
    require!(
        market.get_fixed()?.is_paused == 0,
        YdeltaError::MarketPaused,
        "market is paused"
    )?;
    Ok(())
}

/// Reject state-mutating vault ixs when `GlobalVaultFixed.is_paused == 1`.
pub(crate) fn require_vault_not_paused(
    vault: &YdeltaAccountInfo<'_, '_, crate::state::vault::GlobalVaultFixed>,
) -> Result<(), ProgramError> {
    require!(
        vault.get_fixed()?.is_paused == 0,
        YdeltaError::VaultPaused,
        "vault is paused"
    )?;
    Ok(())
}

/// Reject crosses where the vault's mint does not equal
/// `MarketFixed.debt_mint` (a vault can only fund debt-side asks on
/// markets denominated in its own mint).
pub(crate) fn require_vault_mint_matches_market(
    vault: &YdeltaAccountInfo<'_, '_, crate::state::vault::GlobalVaultFixed>,
    market: &YdeltaAccountInfo<'_, '_, MarketFixed>,
) -> Result<(), ProgramError> {
    // v1 D1: the structural identity is the BANK — a vault can only fund
    // markets whose debt bank is the vault's bank (settlement CPIs assume
    // one bank on both legs). The mint check stays as defense-in-depth.
    let vault_bank = vault.get_fixed()?.lending_pool;
    let market_debt_bank = market.get_fixed()?.debt_lending_pool;
    require!(
        vault_bank == market_debt_bank,
        YdeltaError::VaultWrongMint,
        "vault.lending_pool ({}) does not match market.debt_lending_pool ({})",
        vault_bank,
        market_debt_bank
    )?;
    let vault_mint = vault.get_fixed()?.mint;
    let market_debt_mint = market.get_fixed()?.debt_mint;
    require!(
        vault_mint == market_debt_mint,
        YdeltaError::VaultWrongMint,
        "vault.mint ({}) does not match market.debt_mint ({})",
        vault_mint,
        market_debt_mint
    )?;
    Ok(())
}

/// Account context for `CreateGlobalConfig` (tag 28). One-shot:
/// initializes the singleton `GlobalConfig` PDA. Signer becomes
/// `protocol_admin`.
pub(crate) struct CreateGlobalConfigContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub global_config: &'a AccountInfo<'info>,
    pub system_program: Program<'a, 'info>,
}

impl<'a, 'info> CreateGlobalConfigContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let global_config = next_account_info(account_iter)?;
        let system_program = Program::new(next_account_info(account_iter)?, &system_program::id())?;

        let program_data_ai = next_account_info(account_iter)?;
        let (expected_program_data, _bump) = Pubkey::find_program_address(
            &[crate::ID.as_ref()],
            &solana_program::bpf_loader_upgradeable::id(),
        );
        require!(
            *program_data_ai.key == expected_program_data,
            YdeltaError::IncorrectAccount,
            "program_data account does not match the BPF upgrade-loader \
             ProgramData PDA for ydelta"
        )?;
        require!(
            *program_data_ai.owner == solana_program::bpf_loader_upgradeable::id(),
            YdeltaError::IncorrectAccount,
            "program_data account is not owned by BpfLoaderUpgradeable"
        )?;
        let data = program_data_ai.try_borrow_data()?;
        require!(
            data.len() >= 45,
            YdeltaError::IncorrectAccount,
            "program_data account too small ({} bytes)",
            data.len()
        )?;
        let tag = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        require!(
            tag == 3,
            YdeltaError::IncorrectAccount,
            "program_data variant tag {} is not ProgramData (3)",
            tag
        )?;
        let auth_tag = data[12];
        require!(
            auth_tag == 1,
            YdeltaError::ProtocolAdminRequired,
            "program has no upgrade authority — bootstrap of protocol_admin \
             requires an upgradeable deploy"
        )?;
        let mut auth_bytes = [0u8; 32];
        auth_bytes.copy_from_slice(&data[13..45]);
        let upgrade_authority = Pubkey::new_from_array(auth_bytes);
        drop(data);
        require!(
            *payer.info.key == upgrade_authority,
            YdeltaError::ProtocolAdminRequired,
            "create_global_config: signer {} != program upgrade authority {}",
            payer.info.key,
            upgrade_authority
        )?;

        Ok(Self {
            payer,
            global_config,
            system_program,
        })
    }
}

/// Account context for `TransferProtocolAdmin` (tag 29). Two-step
/// protocol-admin handoff on `GlobalConfig`.
pub(crate) struct TransferProtocolAdminContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub global_config: YdeltaAccountInfo<'a, 'info, crate::state::global_config::GlobalConfig>,
}

impl<'a, 'info> TransferProtocolAdminContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let global_config = load_global_config_no_pause(account_iter)?;
        Ok(Self {
            payer,
            global_config,
        })
    }
}

/// Account context for `AcceptProtocolAdmin` (tag 30).
pub(crate) struct AcceptProtocolAdminContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub global_config: YdeltaAccountInfo<'a, 'info, crate::state::global_config::GlobalConfig>,
}

impl<'a, 'info> AcceptProtocolAdminContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let global_config = load_global_config_no_pause(account_iter)?;
        Ok(Self {
            payer,
            global_config,
        })
    }
}

/// Account context for `SetGlobalPause` (tag 31). Protocol-admin
/// gated; flips `GlobalConfig.is_paused`.
pub(crate) struct SetGlobalPauseContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub global_config: YdeltaAccountInfo<'a, 'info, crate::state::global_config::GlobalConfig>,
}

impl<'a, 'info> SetGlobalPauseContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let global_config = load_global_config_no_pause(account_iter)?;
        Ok(Self {
            payer,
            global_config,
        })
    }
}

/// Reject state-mutating ixs whenever `GlobalConfig.is_paused == 1`.
pub(crate) fn require_global_not_paused(
    global_config: &YdeltaAccountInfo<'_, '_, crate::state::global_config::GlobalConfig>,
) -> Result<(), ProgramError> {
    require!(
        global_config.get_fixed()?.is_paused == 0,
        YdeltaError::GlobalPaused,
        "global pause active"
    )?;
    Ok(())
}

/// Pull the singleton `GlobalConfig` PDA off the account iterator and
/// reject the call if the global pause flag is set.
pub(crate) fn load_global_config<'a, 'info>(
    account_iter: &mut Iter<'a, AccountInfo<'info>>,
) -> Result<YdeltaAccountInfo<'a, 'info, crate::state::global_config::GlobalConfig>, ProgramError> {
    let global_config = load_global_config_no_pause(account_iter)?;
    require_global_not_paused(&global_config)?;
    Ok(global_config)
}

/// Same as [`load_global_config`] but skips the pause gate — used by
/// the protocol-admin transfer / pause ixs so admin can recover a
/// paused program.
pub(crate) fn load_global_config_no_pause<'a, 'info>(
    account_iter: &mut Iter<'a, AccountInfo<'info>>,
) -> Result<YdeltaAccountInfo<'a, 'info, crate::state::global_config::GlobalConfig>, ProgramError> {
    let ai = next_account_info(account_iter)?;
    let (expected, _) = crate::state::global_config::global_config_pda();
    require!(
        *ai.key == expected,
        YdeltaError::IncorrectAccount,
        "global_config PDA mismatch"
    )?;
    let global_config = YdeltaAccountInfo::<crate::state::global_config::GlobalConfig>::new(ai)?;
    Ok(global_config)
}

#[allow(dead_code)]
/// Account context for `ConvertP2PoolToFixed` (tag 33). Borrower-side
/// upgrade: takes the live P2Pool loan, both market integration
/// accounts, the vault settle accounts for each crossed ask, and the
/// oracle pair so the new loans pass the LTV gate.
pub(crate) struct ConvertP2PoolToFixedContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub loan: YdeltaAccountInfo<'a, 'info, crate::state::loan::LoanFixed>,

    pub system_program: Program<'a, 'info>,
    pub borrower_marginfi_account: MarginfiAccountInfo<'a, 'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub debt_liquidity_vault: TokenAccountInfo<'a, 'info>,
    pub debt_bank_lva: &'a AccountInfo<'info>,

    pub debt_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub collateral_bank: MarginfiBankInfo<'a, 'info>,

    pub collateral_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub market_debt_vault: TokenAccountInfo<'a, 'info>,
    pub debt_mint: MintAccountInfo<'a, 'info>,
    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub token_program: TokenProgram<'a, 'info>,
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,

    pub global_vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,

    pub global_vault_integration_account: MarginfiAccountInfo<'a, 'info>,

    pub global_vault_signer: &'a AccountInfo<'info>,

    pub global_vault_signer_bump: u8,

    pub cranker_refund: &'a AccountInfo<'info>,
}

impl<'a, 'info> ConvertP2PoolToFixedContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let loan = YdeltaAccountInfo::<crate::state::loan::LoanFixed>::new(next_account_info(
            account_iter,
        )?)?;
        let system_program = Program::new(next_account_info(account_iter)?, &system_program::id())?;

        let market_key = *market.key;
        {
            let l = loan.get_fixed()?;
            require!(
                l.market == market_key,
                YdeltaError::IncorrectAccount,
                "loan.market does not match passed-in market"
            )?;
            require!(
                l.loan_type == crate::state::loan::LoanType::P2Pool as u8,
                YdeltaError::InvalidArgument,
                "convert_p2pool_to_fixed: loan_type is {} (expected P2Pool=1)",
                l.loan_type
            )?;
            require!(
                l.state == crate::state::loan::LoanState::Active as u8,
                YdeltaError::InvalidArgument,
                "convert_p2pool_to_fixed: loan must be Active (state={})",
                l.state
            )?;

            let (expected_loan, _bump) =
                crate::state::loan::loan_pda(&market_key, l.matched_loan_sequence);
            require!(
                *loan.info.key == expected_loan,
                YdeltaError::IncorrectAccount,
                "loan PDA does not match [b\"loan\", market, sequence={}]",
                l.matched_loan_sequence
            )?;
        }

        let market_fixed: Ref<MarketFixed> = market.get_fixed()?;
        let debt_mint_pk: Pubkey = market_fixed.debt_mint;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let collateral_lending_pool: Pubkey = market_fixed.collateral_lending_pool;
        let borrower_integration_pk: Pubkey = market_fixed.borrower_integration_account;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
        let market_debt_vault_pk: Pubkey = market_fixed.debt_vault;
        let expected_marginfi_group: Pubkey = market_fixed.marginfi_group;
        drop(market_fixed);

        let borrower_mfi_ai = next_account_info(account_iter)?;
        require!(
            *borrower_mfi_ai.key == borrower_integration_pk,
            YdeltaError::IncorrectAccount,
            "borrower_marginfi_account does not match per-market PDA"
        )?;

        let debt_bank_ai = next_account_info(account_iter)?;
        require!(
            *debt_bank_ai.key == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "debt_bank does not match market.debt_lending_pool"
        )?;
        let debt_liquidity_vault_ai = next_account_info(account_iter)?;
        let debt_bank_lva_ai = next_account_info(account_iter)?;

        let debt_oracle_ais = MarginfiOracleAis::load(account_iter, debt_bank_ai)?;
        let collateral_bank_ai = next_account_info(account_iter)?;
        require!(
            *collateral_bank_ai.key == collateral_lending_pool,
            YdeltaError::IncorrectAccount,
            "collateral_bank does not match market.collateral_lending_pool"
        )?;
        let collateral_oracle_ais = MarginfiOracleAis::load(account_iter, collateral_bank_ai)?;

        let market_debt_vault_ai = next_account_info(account_iter)?;
        require!(
            *market_debt_vault_ai.key == market_debt_vault_pk,
            YdeltaError::IncorrectAccount,
            "market_debt_vault PDA mismatch"
        )?;
        let debt_mint_ai = next_account_info(account_iter)?;
        require!(
            *debt_mint_ai.key == debt_mint_pk,
            YdeltaError::IncorrectAccount,
            "debt_mint mismatch"
        )?;

        let market_signer_ai = next_account_info(account_iter)?;
        require!(
            *market_signer_ai.key == market_signer_pk,
            YdeltaError::IncorrectAccount,
            "market_signer mismatch"
        )?;

        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let group_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;

        {
            let bd = debt_bank_ai.try_borrow_data()?;
            let bank = marginfi_mocks::state::Bank::try_from_account_data(&bd)
                .map_err(|_| YdeltaError::IncorrectAccount)?;
            require!(
                *debt_liquidity_vault_ai.key == bank.liquidity_vault,
                YdeltaError::IncorrectAccount,
                "debt_liquidity_vault mismatch"
            )?;
        }
        let (expected_lva, _) = Pubkey::find_program_address(
            &[b"liquidity_vault_auth", debt_bank_ai.key.as_ref()],
            marginfi_program.info.key,
        );
        require!(
            *debt_bank_lva_ai.key == expected_lva,
            YdeltaError::IncorrectAccount,
            "debt_bank_lva PDA mismatch"
        )?;

        let market_debt_vault = TokenAccountInfo::new_with_owner(
            market_debt_vault_ai,
            &debt_mint_pk,
            &market_signer_pk,
        )?;
        let debt_liquidity_vault = TokenAccountInfo::new(debt_liquidity_vault_ai, &debt_mint_pk)?;
        let debt_mint = MintAccountInfo::new(debt_mint_ai)?;

        let debt_bank = MarginfiBankInfo::new_with_expected_group(
            debt_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;
        let collateral_bank = MarginfiBankInfo::new_with_expected_group(
            collateral_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;
        let borrower_marginfi_account = MarginfiAccountInfo::new_with_expected_authority_and_group(
            borrower_mfi_ai,
            marginfi_program.info.key,
            &market_signer_pk,
            &expected_marginfi_group,
        )?;
        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        require!(
            *group_ai.key == expected_marginfi_group,
            YdeltaError::IncorrectAccount,
            "marginfi_group does not match MarketFixed.marginfi_group"
        )?;

        let global_vault_ai = next_account_info(account_iter)?;
        let global_vault =
            YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(global_vault_ai)?;
        require_vault_not_paused(&global_vault)?;
        let (expected_global_vault, _) =
            crate::state::vault::global_vault_pda(&debt_lending_pool);
        require!(
            *global_vault_ai.key == expected_global_vault,
            YdeltaError::IncorrectAccount,
            "global_vault PDA does not match expected derivation from \
             market.debt_lending_pool (v1 D1: bank-keyed)"
        )?;

        let (vault_integration_pk, vault_signer_bump, vault_lending_pool) = {
            let vfixed = global_vault.get_fixed()?;
            (
                vfixed.integration_account,
                vfixed.global_vault_signer_bump,
                vfixed.lending_pool,
            )
        };
        require!(
            vault_lending_pool == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "global_vault.lending_pool does not match market.debt_lending_pool"
        )?;
        let global_vault_signer_ai = next_account_info(account_iter)?;
        let (expected_vault_signer, _) =
            crate::state::vault::global_vault_signer_pda(global_vault_ai.key);
        require!(
            *global_vault_signer_ai.key == expected_vault_signer,
            YdeltaError::IncorrectAccount,
            "global_vault_signer PDA does not match [GLOBAL_VAULT_SIGNER_SEED, global_vault]"
        )?;
        let global_vault_integration_ai = next_account_info(account_iter)?;
        require!(
            *global_vault_integration_ai.key == vault_integration_pk,
            YdeltaError::IncorrectAccount,
            "global_vault_integration_account does not match vault header"
        )?;
        let global_vault_integration_account =
            MarginfiAccountInfo::new_with_expected_authority_and_group(
                global_vault_integration_ai,
                marginfi_program.info.key,
                &expected_vault_signer,
                &expected_marginfi_group,
            )?;

        let cranker_refund_ai = next_account_info(account_iter)?;
        let loan_created_by: Pubkey = loan.get_fixed()?.created_by;
        require!(
            *cranker_refund_ai.key == loan_created_by,
            YdeltaError::IncorrectAccount,
            "cranker_refund {} does not match loan.created_by {}",
            cranker_refund_ai.key,
            loan_created_by
        )?;
        let cranker_refund = cranker_refund_ai;

        Ok(Self {
            payer,
            market,
            loan,
            system_program,
            borrower_marginfi_account,
            debt_bank,
            debt_liquidity_vault,
            debt_bank_lva: debt_bank_lva_ai,
            debt_oracle_ais,
            collateral_bank,
            collateral_oracle_ais,
            market_debt_vault,
            debt_mint,
            market_signer: market_signer_ai,
            market_signer_bump,
            token_program,
            marginfi_group,
            marginfi_program,
            global_vault,
            global_vault_integration_account,
            global_vault_signer: global_vault_signer_ai,
            global_vault_signer_bump: vault_signer_bump,
            cranker_refund,
        })
    }
}

#[allow(dead_code)]
/// Account context for `CheckLtvLiquidatable` (tag 34). Read-only
/// simulation; loads the loan + both oracles to reproduce the
/// maintenance-LTV gate `LiquidateLoan` enforces.
pub(crate) struct CheckLtvLiquidatableContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub loan: YdeltaAccountInfo<'a, 'info, crate::state::loan::LoanFixed>,

    pub borrower_marginfi_account: MarginfiAccountInfo<'a, 'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub debt_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub collateral_bank: MarginfiBankInfo<'a, 'info>,
    pub collateral_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
}

impl<'a, 'info> CheckLtvLiquidatableContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let loan = YdeltaAccountInfo::<crate::state::loan::LoanFixed>::new(next_account_info(
            account_iter,
        )?)?;

        let market_key = *market.key;
        let loan_sequence: u64 = loan.get_fixed()?.matched_loan_sequence;
        let (expected_loan, _bump) = crate::state::loan::loan_pda(&market_key, loan_sequence);
        require!(
            *loan.info.key == expected_loan,
            YdeltaError::IncorrectAccount,
            "loan PDA mismatch"
        )?;
        require!(
            loan.get_fixed()?.market == market_key,
            YdeltaError::IncorrectAccount,
            "loan.market does not match passed market"
        )?;

        let market_fixed: Ref<MarketFixed> = market.get_fixed()?;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let collateral_lending_pool: Pubkey = market_fixed.collateral_lending_pool;
        let borrower_integration_pk: Pubkey = market_fixed.borrower_integration_account;
        let expected_marginfi_group: Pubkey = market_fixed.marginfi_group;
        drop(market_fixed);

        let borrower_mfi_ai = next_account_info(account_iter)?;
        require!(
            *borrower_mfi_ai.key == borrower_integration_pk,
            YdeltaError::IncorrectAccount,
            "borrower_marginfi_account does not match per-market PDA"
        )?;

        let debt_bank_ai = next_account_info(account_iter)?;
        require!(
            *debt_bank_ai.key == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "debt_bank does not match market.debt_lending_pool"
        )?;
        let debt_oracle_ais = MarginfiOracleAis::load(account_iter, debt_bank_ai)?;

        let collateral_bank_ai = next_account_info(account_iter)?;
        require!(
            *collateral_bank_ai.key == collateral_lending_pool,
            YdeltaError::IncorrectAccount,
            "collateral_bank does not match market.collateral_lending_pool"
        )?;
        let collateral_oracle_ais = MarginfiOracleAis::load(account_iter, collateral_bank_ai)?;

        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;

        let borrower_marginfi_account = MarginfiAccountInfo::new_with_expected_group(
            borrower_mfi_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;
        let debt_bank = MarginfiBankInfo::new_with_expected_group(
            debt_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;
        let collateral_bank = MarginfiBankInfo::new_with_expected_group(
            collateral_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;

        Ok(Self {
            payer,
            market,
            loan,
            borrower_marginfi_account,
            debt_bank,
            debt_oracle_ais,
            collateral_bank,
            collateral_oracle_ais,
            marginfi_program,
        })
    }
}

#[allow(dead_code)]
/// Account context for `CheckMaturityLiquidatable` (tag 35). Read-only
/// simulation; loads the loan to reproduce the maturity + grace gate
/// `SettleMaturedLoan` enforces.
pub(crate) struct CheckMaturityLiquidatableContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub loan: YdeltaAccountInfo<'a, 'info, crate::state::loan::LoanFixed>,
    pub borrower_marginfi_account: MarginfiAccountInfo<'a, 'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
}

impl<'a, 'info> CheckMaturityLiquidatableContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let loan = YdeltaAccountInfo::<crate::state::loan::LoanFixed>::new(next_account_info(
            account_iter,
        )?)?;

        let market_key = *market.key;
        let loan_sequence: u64 = loan.get_fixed()?.matched_loan_sequence;
        let (expected_loan, _bump) = crate::state::loan::loan_pda(&market_key, loan_sequence);
        require!(
            *loan.info.key == expected_loan,
            YdeltaError::IncorrectAccount,
            "loan PDA mismatch"
        )?;
        require!(
            loan.get_fixed()?.market == market_key,
            YdeltaError::IncorrectAccount,
            "loan.market does not match passed market"
        )?;

        let market_fixed: Ref<MarketFixed> = market.get_fixed()?;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let borrower_integration_pk: Pubkey = market_fixed.borrower_integration_account;
        let expected_marginfi_group: Pubkey = market_fixed.marginfi_group;
        drop(market_fixed);

        let borrower_mfi_ai = next_account_info(account_iter)?;
        require!(
            *borrower_mfi_ai.key == borrower_integration_pk,
            YdeltaError::IncorrectAccount,
            "borrower_marginfi_account does not match per-market PDA"
        )?;

        let debt_bank_ai = next_account_info(account_iter)?;
        require!(
            *debt_bank_ai.key == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "debt_bank does not match market.debt_lending_pool"
        )?;

        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;

        let borrower_marginfi_account = MarginfiAccountInfo::new_with_expected_group(
            borrower_mfi_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;
        let debt_bank = MarginfiBankInfo::new_with_expected_group(
            debt_bank_ai,
            marginfi_program.info.key,
            &expected_marginfi_group,
        )?;

        Ok(Self {
            payer,
            market,
            loan,
            borrower_marginfi_account,
            debt_bank,
            marginfi_program,
        })
    }
}
