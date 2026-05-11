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

/// Account list for `CreateMarket`. Order matches `YdeltaInstruction`.
///
/// The tail carries five integration accounts: `marginfi_group + debt_bank +
/// collateral_bank + marginfi_account + marginfi_program`. Each gets a typed
/// wrapper that validates ownership and discriminator at load time — the
/// processor sees only the validated `&AccountInfo` and runs business-rule
/// checks on top.
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
    // ─── Integration with the underlying lending protocol ───
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub collateral_bank: MarginfiBankInfo<'a, 'info>,
    /// Lender-side marginfi-account PDA at `[b"marginfi_account",
    /// market]`. Uninitialised at create_market time — the processor
    /// CPIs `marginfi_account_initialize` for it.
    pub lender_marginfi_account: &'a AccountInfo<'info>,
    pub lender_marginfi_account_bump: u8,
    /// Borrower-side marginfi-account PDA at
    /// `[b"borrower_marginfi_account", market]`. Same uninit-at-create
    /// semantics as the lender-side one.
    pub borrower_marginfi_account: &'a AccountInfo<'info>,
    pub borrower_marginfi_account_bump: u8,
    /// Market-signer PDA — authority on both marginfi-accounts.
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

        // Integration accounts. On-wire order is:
        //   group, debt_bank, collateral_bank,
        //   lender_marginfi_account, borrower_marginfi_account,
        //   market_signer, marginfi_program
        // marginfi_program validates first so the typed wrappers can
        // check ownership. The two marginfi accounts and market_signer
        // are uninitialised system accounts at this point — we only
        // verify their addresses match the derived PDAs.
        let group_ai = next_account_info(account_iter)?;
        let debt_bank_ai = next_account_info(account_iter)?;
        let collateral_bank_ai = next_account_info(account_iter)?;
        let lender_mfi_acct_ai = next_account_info(account_iter)?;
        let borrower_mfi_acct_ai = next_account_info(account_iter)?;
        let market_signer_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;
        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        let debt_bank = MarginfiBankInfo::new(debt_bank_ai, marginfi_program.info.key)?;
        let collateral_bank = MarginfiBankInfo::new(collateral_bank_ai, marginfi_program.info.key)?;

        // Bind each bank's mint to the market's mint. Without this
        // check, a privileged admin could stand up a market whose
        // banks serve foreign mints — every subsequent deposit /
        // withdraw would silently route atoms to the wrong bank.
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

/// Account list for `ClaimSeat`.
#[allow(dead_code)] // load-bearing fields validated at load time
pub(crate) struct ClaimSeatContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub system_program: Program<'a, 'info>,
    /// Signer's `UserAccount`. Auto-created if missing
    /// (signer pays rent). Mirror updated in `process_claim_seat`
    /// after the canonical `ClaimedSeat` insert.
    pub user_account: YdeltaAccountInfo<'a, 'info, crate::state::user_account::UserAccountFixed>,
    /// Raw AccountInfo retained because `expand_user_account` /
    /// `ensure_user_account_for_signer` need the signer ↔ AccountInfo
    /// pair for the system_program::transfer rent top-up.
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
        let system_program = Program::new(system_program_ai, &system_program::id())?;
        // Append signer's UserAccount as the 4th account.
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

/// Account list for `Deposit`. Atoms flow user → market SPL vault → bank
/// liquidity vault. The market's SPL vault sits between the user (who signs
/// the first SPL transfer) and marginfi (whose deposit CPI requires
/// `signer_token_account.owner == authority`, i.e. `market_signer`). The
/// vault is owned by `market_signer`, so ydelta signs the marginfi-side CPI
/// for it.
pub(crate) struct DepositContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub trader_token: TokenAccountInfo<'a, 'info>,
    /// Per-market staging vault, owned by `market_signer`. Atoms hop
    /// through here on the way to the bank's liquidity vault.
    pub vault: TokenAccountInfo<'a, 'info>,
    pub token_program: TokenProgram<'a, 'info>,
    pub mint: MintAccountInfo<'a, 'info>,
    pub is_debt: bool,
    // ─── Integration with the underlying lending protocol ───
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub marginfi_account: MarginfiAccountInfo<'a, 'info>,
    pub bank: MarginfiBankInfo<'a, 'info>,
    pub liquidity_vault: TokenAccountInfo<'a, 'info>,
    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
    // ─── UserAccount mirror ───
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
        // Deposits route to the side-relevant marginfi-account. Lender
        // USDC → lender_integration_account (asset only). Borrower
        // wSOL → borrower_integration_account (asset for collateral;
        // pairs with future P2Pool liability on the debt bank without
        // per-bank overlap).
        let lender_integration_account: Pubkey = market_fixed.lender_integration_account;
        let borrower_integration_account: Pubkey = market_fixed.borrower_integration_account;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
        drop(market_fixed);

        let token_account_info = next_account_info(account_iter)?;
        let token_mint_bytes = token_account_info.try_borrow_data()?;
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
        // The market's SPL staging vault for this side. Owned by
        // `market_signer`. Verified by deriving the expected PDA at
        // `[b"vault", market, mint]`.
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

        // Integration accounts. The marginfi-account passed here MUST
        // be the side-relevant one (lender for debt deposits, borrower
        // for collateral deposits).
        let group_ai = next_account_info(account_iter)?;
        let mfi_acct_ai = next_account_info(account_iter)?;
        let bank_ai = next_account_info(account_iter)?;
        let liquidity_vault_ai = next_account_info(account_iter)?;
        let market_signer_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;

        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        let marginfi_account = MarginfiAccountInfo::new(mfi_acct_ai, marginfi_program.info.key)?;
        let bank = MarginfiBankInfo::new(bank_ai, marginfi_program.info.key)?;

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

        // The bank's liquidity vault key is recorded on the bank header.
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

        // Append signer's UserAccount + system_program (system_program
        // is needed for auto-create on first call).
        let user_account_ai = next_account_info(account_iter)?;
        let system_program_ai = next_account_info(account_iter)?;
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

/// Account list for `Withdraw`. Routes atoms through marginfi via CPI.
/// Withdraw is health-checking, so the loader collects BOTH banks + BOTH
/// oracles up front; the processor builds the marginfi `remaining_accounts`
/// list dynamically based on which balances are currently active on the
/// market's marginfi-account.
pub(crate) struct WithdrawContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub trader_token: TokenAccountInfo<'a, 'info>,
    /// Per-market staging vault, owned by `market_signer`. Marginfi
    /// withdraws atoms here; ydelta then SPL-transfers vault → user.
    pub vault: TokenAccountInfo<'a, 'info>,
    pub vault_bump: u8,
    pub token_program: TokenProgram<'a, 'info>,
    pub mint: MintAccountInfo<'a, 'info>,
    pub is_debt: bool,
    // ─── Integration with the underlying lending protocol ───
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub marginfi_account: MarginfiAccountInfo<'a, 'info>,
    /// Bank for the side being withdrawn (alias of debt_bank or
    /// collateral_bank).
    pub bank: MarginfiBankInfo<'a, 'info>,
    pub liquidity_vault: TokenAccountInfo<'a, 'info>,
    /// Marginfi PDA at `[b"liquidity_vault_auth", bank_pk]` — signs the
    /// vault → destination transfer inside marginfi's withdraw. Ydelta
    /// passes it through; marginfi re-derives and signs.
    pub bank_liquidity_vault_authority: &'a AccountInfo<'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub collateral_bank: MarginfiBankInfo<'a, 'info>,
    /// Variadic oracle slice for `debt_bank`. See [`MarginfiOracleAis`].
    pub debt_oracle_ais: MarginfiOracleAis<'a, 'info>,
    /// Variadic oracle slice for `collateral_bank`. See [`MarginfiOracleAis`].
    pub collateral_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
    /// Signer's UserAccount mirror.
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
        // Side-relevant marginfi-account routing.
        let lender_integration_account: Pubkey = market_fixed.lender_integration_account;
        let borrower_integration_account: Pubkey = market_fixed.borrower_integration_account;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
        drop(market_fixed);

        let token_account_info = next_account_info(account_iter)?;
        let token_mint_bytes = token_account_info.try_borrow_data()?;
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
        // Per-market staging vault for this side, owned by `market_signer`.
        let vault_ai = next_account_info(account_iter)?;
        let (expected_vault, vault_bump) = get_vault_address(market.key, &mint_key);
        let vault = TokenAccountInfo::new_with_owner_and_key(
            vault_ai,
            &mint_key,
            &market_signer_pk,
            &expected_vault,
        )?;
        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let mint = MintAccountInfo::new(next_account_info(account_iter)?)?;

        // Integration accounts.
        let group_ai = next_account_info(account_iter)?;
        let mfi_acct_ai = next_account_info(account_iter)?;
        let debt_bank_ai = next_account_info(account_iter)?;
        let collateral_bank_ai = next_account_info(account_iter)?;
        let liquidity_vault_ai = next_account_info(account_iter)?;
        let bank_liquidity_vault_authority_ai = next_account_info(account_iter)?;
        // Variadic oracle slice for debt_bank, then collateral_bank. Each
        // entry is pubkey-pinned against `bank.config.oracle_keys[i]` by
        // `MarginfiOracleAis::load`.
        let debt_oracle_ais = MarginfiOracleAis::load(account_iter, debt_bank_ai)?;
        let collateral_oracle_ais = MarginfiOracleAis::load(account_iter, collateral_bank_ai)?;
        let market_signer_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;

        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        let marginfi_account = MarginfiAccountInfo::new(mfi_acct_ai, marginfi_program.info.key)?;
        let debt_bank = MarginfiBankInfo::new(debt_bank_ai, marginfi_program.info.key)?;
        let collateral_bank = MarginfiBankInfo::new(collateral_bank_ai, marginfi_program.info.key)?;

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

        // Oracle pubkeys are pinned by `MarginfiOracleAis::load`
        // (per-slot match against `bank.config.oracle_keys[i]`).
        // Setups that need multiple oracles (Kamino/Drift/Solend
        // wrappers, StakedWithPythPush) flow through end-to-end; the
        // decoder rejects setups it doesn't yet know how to read.

        // The bank for this side is one of debt_bank/collateral_bank.
        let bank = if is_debt {
            debt_bank.clone()
        } else {
            collateral_bank.clone()
        };

        // Liquidity vault must match the side's bank.liquidity_vault.
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

        // Sanity-check the vault authority pubkey matches the marginfi
        // PDA at `[b"liquidity_vault_auth", bank_pk]`.
        let (expected_vault_authority, _) = Pubkey::find_program_address(
            &[b"liquidity_vault_auth", bank.info.key.as_ref()],
            marginfi_program.info.key,
        );
        require!(
            *bank_liquidity_vault_authority_ai.key == expected_vault_authority,
            YdeltaError::IncorrectAccount,
            "bank_liquidity_vault_authority does not match marginfi PDA"
        )?;

        // Append signer's UserAccount + system_program.
        let user_account_ai = next_account_info(account_iter)?;
        let system_program_ai = next_account_info(account_iter)?;
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
            vault_bump,
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

/// Shared account shape for `CancelOrder` and `UpdateOrder`. Pure seat-
/// balance bookkeeping — no token accounts, no marginfi CPI, no oracle
/// reads. `PlaceOrder` is a separate context (it carries marginfi/oracle
/// accounts for LTV-at-match + P2Pool fallback); `cancel` and `update`
/// keep the simpler 3-account form.
pub(crate) struct OrderContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub _system_program: Program<'a, 'info>,
    /// Signer's UserAccount mirror.
    pub user_account_ai: &'a AccountInfo<'info>,
    /// OPTIONAL trailing loan account, supplied only when canceling a
    /// `SecondaryLoanSale` bid so the processor can clear the loan's
    /// `has_resting_secondary_bid` flag (O(1) duplicate-check
    /// counterpart). For primary cancels this is `None`.
    pub secondary_loan_ai: Option<&'a AccountInfo<'info>>,
}

impl<'a, 'info> OrderContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let _system_program =
            Program::new(next_account_info(account_iter)?, &system_program::id())?;

        // Signer's UserAccount + system_program for auto-create.
        let user_account_ai = next_account_info(account_iter)?;
        let system_program_ai = next_account_info(account_iter)?;
        let _ = crate::validation::user_account::ensure_user_account_for_signer(
            &payer,
            user_account_ai,
            system_program_ai,
        )?;

        // Optional trailing loan account for secondary-bid cancels.
        let secondary_loan_ai = account_iter.next();

        Ok(Self {
            payer,
            market,
            _system_program,
            user_account_ai,
            secondary_loan_ai,
        })
    }
}

/// Account list for `PlaceOrder`. Carries the marginfi accounts the
/// matching engine needs for LTV-at-match and the P2Pool fallback CPI:
/// both banks, both oracles, market_signer + marginfi_program + the
/// debt-side liquidity_vault + bank_liquidity_vault_authority for the
/// residual borrow CPI.
pub(crate) struct PlaceOrderContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub _system_program: Program<'a, 'info>,
    // ─── Marginfi accounts on place_order ───
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    /// place_order's marginfi-account is the **borrower-side**
    /// account. The P2Pool fallback fires `marginfi.borrow` against
    /// this account; the borrower's collateral asset already lives
    /// here, so marginfi's solvency check on the new liability sees
    /// the backing.
    pub borrower_marginfi_account: MarginfiAccountInfo<'a, 'info>,
    /// Lender-side marginfi-account at
    /// `MarketFixed.lender_integration_account`. Destination of the
    /// P2Pool deposit-back CPI (atoms borrowed from marginfi land
    /// here as ASSET shares; ydelta credits the share delta to the
    /// borrower's `ClaimedSeat.debt_withdrawable_shares` so the
    /// borrower can withdraw to atoms later via `process_withdraw`).
    /// Mirrors the Fixed-loan claim model.
    pub lender_marginfi_account: MarginfiAccountInfo<'a, 'info>,
    /// Market's debt-mint vault PDA at `[b"vault", market, debt_mint]`.
    /// Authority = market_signer. Used as the staging token account
    /// for the P2Pool deposit-back: `marginfi.borrow` lands atoms here
    /// (writable destination) and the immediately-following
    /// `marginfi.deposit` reads them out signed by market_signer.
    pub market_debt_vault: TokenAccountInfo<'a, 'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub collateral_bank: MarginfiBankInfo<'a, 'info>,
    /// Variadic oracle slice for `debt_bank`. See [`MarginfiOracleAis`].
    pub debt_oracle_ais: MarginfiOracleAis<'a, 'info>,
    /// Variadic oracle slice for `collateral_bank`. See [`MarginfiOracleAis`].
    pub collateral_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
    /// Debt liquidity vault — destination of the P2Pool borrow CPI.
    /// Validated against `debt_bank.liquidity_vault`.
    pub debt_liquidity_vault: TokenAccountInfo<'a, 'info>,
    /// Marginfi PDA at `[b"liquidity_vault_auth", debt_bank.key]`.
    pub debt_bank_liquidity_vault_authority: &'a AccountInfo<'info>,
    /// Destination of the `marginfi.borrow` CPI when a Bid's residual
    /// triggers a P2Pool borrow. The borrower's debt-mint wallet token
    /// account — atoms land directly here so the borrower doesn't need
    /// a separate claim ix. Owned by `payer`.
    pub borrower_debt_token: TokenAccountInfo<'a, 'info>,
    /// SPL token program for the borrower's debt-mint ATA. Marginfi's
    /// `lending_account_borrow` does the SPL transfer internally.
    pub token_program: TokenProgram<'a, 'info>,
    /// Signer's UserAccount mirror.
    pub user_account_ai: &'a AccountInfo<'info>,
    /// Present only when `kind == SecondaryLoanSale`. The existing
    /// Loan PDA being put up for sale by the seller (signer). Loader
    /// validates: market match, `loan_type == Fixed`, `outstanding > 0`,
    /// `lender_seat_index == signer's seat`. The matching engine works
    /// off snapshot fields stamped onto the `RestingOrder` at placement;
    /// the cranker re-reads the loan at finalization (staleness
    /// defense). `None` for primary orders.
    pub secondary_loan: Option<YdeltaAccountInfo<'a, 'info, crate::state::loan::LoanFixed>>,
    /// Optional GlobalVault account. Required only when the matching
    /// engine crosses a vault-owned maker; without it, those crosses
    /// are skipped (Manifest-style `GlobalSkip`). The processor
    /// applies match-time encumbrance to vault state in the same tx
    /// so concurrent matches in other markets see the locked idle
    /// pool.
    pub vault: Option<&'a AccountInfo<'info>>,
}

impl<'a, 'info> PlaceOrderContext<'a, 'info> {
    /// Loader for `place_order`. `kind` decides whether an extra Loan
    /// PDA account is consumed at the end of the account list
    /// (`SecondaryLoanSale` path). For `Primary` orders no trailing
    /// loan account is consumed.
    pub fn load(
        accounts: &'a [AccountInfo<'info>],
        kind: crate::state::OrderKind,
    ) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let _system_program =
            Program::new(next_account_info(account_iter)?, &system_program::id())?;

        // Read MarketFixed integration pubkeys + signer bump up front.
        let market_fixed: Ref<MarketFixed> = market.get_fixed()?;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let collateral_lending_pool: Pubkey = market_fixed.collateral_lending_pool;
        // place_order's marginfi-account is the borrower side (where
        // the P2Pool borrow CPI fires; collateral assets backing the
        // borrow already live there).
        let borrower_integration_account: Pubkey = market_fixed.borrower_integration_account;
        let lender_integration_account: Pubkey = market_fixed.lender_integration_account;
        let market_debt_vault_pk: Pubkey = market_fixed.debt_vault;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
        let debt_mint_pk: Pubkey = market_fixed.debt_mint;
        drop(market_fixed);

        // Marginfi accounts — strict order on the wire.
        let group_ai = next_account_info(account_iter)?;
        let mfi_acct_ai = next_account_info(account_iter)?;
        let debt_bank_ai = next_account_info(account_iter)?;
        let collateral_bank_ai = next_account_info(account_iter)?;
        // Variadic oracle slices, count read from each bank's `OracleSetup`.
        // Pubkey-pinned per-slot against `bank.config.oracle_keys[i]`.
        let debt_oracle_ais = MarginfiOracleAis::load(account_iter, debt_bank_ai)?;
        let collateral_oracle_ais = MarginfiOracleAis::load(account_iter, collateral_bank_ai)?;
        let market_signer_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;
        let debt_liquidity_vault_ai = next_account_info(account_iter)?;
        let debt_bank_liquidity_vault_authority_ai = next_account_info(account_iter)?;
        let borrower_debt_token_ai = next_account_info(account_iter)?;
        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;

        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        let borrower_marginfi_account =
            MarginfiAccountInfo::new(mfi_acct_ai, marginfi_program.info.key)?;
        let debt_bank = MarginfiBankInfo::new(debt_bank_ai, marginfi_program.info.key)?;
        let collateral_bank = MarginfiBankInfo::new(collateral_bank_ai, marginfi_program.info.key)?;

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

        // Validate the debt-side liquidity vault matches debt_bank's record.
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

        // Validate the bank's liquidity-vault authority PDA.
        let (expected_lva, _) = Pubkey::find_program_address(
            &[b"liquidity_vault_auth", debt_bank_ai.key.as_ref()],
            marginfi_program.info.key,
        );
        require!(
            *debt_bank_liquidity_vault_authority_ai.key == expected_lva,
            YdeltaError::IncorrectAccount,
            "debt_bank_liquidity_vault_authority does not match marginfi PDA"
        )?;

        // Oracle pubkeys + count are pinned by `MarginfiOracleAis::load`
        // (per-slot match against `bank.config.oracle_keys[i]`, count
        // read from `BankConfigView::expected_oracle_account_count()`).
        // Multi-oracle setups (Kamino/Drift/Solend wrappers,
        // StakedWithPythPush) flow through end-to-end; the decoder
        // side rejects setups it doesn't yet know how to read.

        // Borrower's debt-mint ATA — destination of the marginfi.borrow
        // CPI on the P2Pool fallback. Owned by the payer so the
        // borrower receives borrowed atoms in their own wallet directly.
        let borrower_debt_token =
            TokenAccountInfo::new_with_owner(borrower_debt_token_ai, &debt_mint_pk, payer.key)?;

        // Signer's UserAccount + system_program for auto-create.
        let user_account_ai = next_account_info(account_iter)?;
        let system_program_ai = next_account_info(account_iter)?;
        let _ = crate::validation::user_account::ensure_user_account_for_signer(
            &payer,
            user_account_ai,
            system_program_ai,
        )?;

        // Lender-side marginfi-account + market's debt-mint vault.
        // Used by the P2Pool deposit-back path to route borrowed atoms
        // through `market.debt_vault` and into
        // `lender_integration_account` so they earn lender-side yield
        // until the borrower withdraws via `process_withdraw`.
        let lender_mfi_acct_ai = next_account_info(account_iter)?;
        let lender_marginfi_account =
            MarginfiAccountInfo::new(lender_mfi_acct_ai, marginfi_program.info.key)?;
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

        // SecondaryLoanSale path consumes one extra account (the Loan
        // PDA being put up for sale) at the end of the list.
        // Validation here is read-only: market match, loan_type=Fixed,
        // outstanding>0, lender owns the loan. The matching engine then
        // snapshots rate/term/principal off the loan onto the
        // RestingOrder; the loan account is NOT carried through into
        // the matching pass.
        let secondary_loan = if kind == crate::state::OrderKind::SecondaryLoanSale {
            let loan_ai = next_account_info(account_iter)?;
            let loan = YdeltaAccountInfo::<crate::state::loan::LoanFixed>::new(loan_ai)?;
            let loan_fixed = loan.get_fixed()?;
            require!(
                loan_fixed.market == *market.key,
                YdeltaError::IncorrectAccount,
                "secondary_loan.market does not match this market"
            )?;
            require!(
                loan_fixed.loan_type == crate::state::loan::LoanType::Fixed as u8,
                YdeltaError::SecondaryLoanWrongType,
                "secondary_loan.loan_type is not Fixed (P2Pool loans cannot be resold)"
            )?;
            // Vault-funded loans run their full tenure. They can only
            // exit via the borrower's repay, post-maturity
            // `settle_matured_loan`, or LTV-breach `liquidate_loan`.
            // Secondary book is wallet-lender only.
            require!(
                loan_fixed.lender_kind == crate::state::OWNER_KIND_USER,
                YdeltaError::SecondaryLoanWrongType,
                "secondary_loan: vault-funded loans cannot be sold on the \
                 secondary book — they hold to maturity / liquidation"
            )?;
            require!(
                loan_fixed.outstanding_debt_atoms > 0,
                YdeltaError::SecondaryLoanSettled,
                "secondary_loan has no outstanding debt to transfer"
            )?;
            // Owner check (signer's seat == loan.lender_seat_index) is
            // done in the processor where the seat tree is reachable;
            // do it there to avoid double-borrowing the market data
            // here.
            drop(loan_fixed);
            Some(loan)
        } else {
            None
        };

        // Optional GlobalVault account at the tail. Always derivable
        // from `debt_mint` so well-behaved callers include it;
        // uninitialized accounts (data_len == 0) signal "no vault on
        // this market" and are treated as `None`.
        //
        // For initialized accounts, we validate three things before
        // accepting them: yDelta-owned, GlobalVaultFixed discriminator,
        // and the PDA derives from this market's debt_mint. Without the
        // last two, a malicious taker could pass any yDelta-owned
        // account (a Loan, another Market, etc.) as the "vault" and
        // `apply_vault_match_deltas` would interpret arbitrary bytes
        // as `GlobalVaultFixed` + walk garbage as a RiskProfile tree.
        let vault: Option<&'a AccountInfo<'info>> = match account_iter.next() {
            Some(ai) => {
                if ai.data_len() == 0 {
                    None
                } else {
                    let _ = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(ai)?;
                    let (expected_vault, _) = crate::state::vault::global_vault_pda(&debt_mint_pk);
                    require!(
                        *ai.key == expected_vault,
                        YdeltaError::IncorrectAccount,
                        "vault PDA does not match expected derivation \
                         from market.debt_mint"
                    )?;
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
            borrower_debt_token,
            token_program,
            user_account_ai,
            secondary_loan,
            vault,
        })
    }
}

/// Resolved cranker dispatch mode, computed by the loader by reading
/// the MatchedLoan queue node's flags. The processor branches on this
/// enum to drive the correct finalization path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessMatchedLoanMode {
    /// Fresh-loan creation. `loan` is empty; processor allocates via
    /// system_program::create_account.
    Primary,
    /// Full secondary transfer. `existing_loan` is the
    /// already-initialized Loan PDA being mutated. No new PDA.
    SecondaryFull,
    /// Partial secondary transfer with split. `existing_loan` is
    /// mutated; `split_loan_to_create` (empty) is allocated for the
    /// buyer's new sub-loan.
    SecondarySplit,
}

/// Account list for `ProcessMatchedLoan`. Permissionless cranker — anyone
/// signs as `payer`. The cranker pays Loan-PDA rent (refunded if the
/// loan ever closes).
///
/// The cranker handles both PRIMARY and SECONDARY-flagged queue nodes.
/// The on-wire account count varies by mode:
/// - PRIMARY: `[payer, market, loan, debt_bank, marginfi_program,
///   system_program]` (6).
/// - SECONDARY full: PRIMARY accounts + `[collateral_bank, debt_oracle,
///   collateral_oracle]` for the cranker-side LTV check (9).
/// - SECONDARY split: SECONDARY full + `[split_loan_to_create]` (10).
///
/// The loader reads the queue node up front to determine `mode` and
/// parses the appropriate account suffix.
pub(crate) struct ProcessMatchedLoanContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    /// Empty Loan PDA being created (mode == Primary). `None` for
    /// SECONDARY paths.
    pub loan: Option<EmptyAccount<'a, 'info>>,
    /// SECONDARY (full or split): existing Loan PDA being mutated.
    /// `None` for primary.
    pub existing_loan: Option<YdeltaAccountInfo<'a, 'info, crate::state::loan::LoanFixed>>,
    /// SECONDARY_SPLIT only: empty Loan PDA allocated for the buyer's
    /// new sub-loan. `None` otherwise.
    pub split_loan_to_create: Option<EmptyAccount<'a, 'info>>,
    /// Bump for the loan PDA being created (primary or split).
    /// Unused for SECONDARY full.
    pub loan_bump: u8,
    /// Debt bank — read by the processor for `amount_to_shares` against
    /// the credited principal.
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    /// SECONDARY paths: collateral bank for LTV recheck. `None` for
    /// primary.
    pub collateral_bank: Option<MarginfiBankInfo<'a, 'info>>,
    /// SECONDARY paths: variadic oracle slices for LTV recheck. `None`
    /// for primary. See [`MarginfiOracleAis`].
    pub debt_oracle_ais: Option<MarginfiOracleAis<'a, 'info>>,
    pub collateral_oracle_ais: Option<MarginfiOracleAis<'a, 'info>>,
    pub system_program: Program<'a, 'info>,
    /// Resolved cranker dispatch mode. Always set.
    pub mode: ProcessMatchedLoanMode,
    /// MatchedLoan queue node, copied at load time so the processor
    /// can re-read without re-borrowing the market data.
    pub queue_node: crate::state::market::MatchedLoan,
    /// Index of the queue node in the MatchedLoan tree. Processor
    /// uses this to remove the node + free the slot after
    /// finalization (or on stale-bid drop).
    pub queue_node_index: hypertree::DataIndex,
    /// Vault settlement bundle. Required when Primary mode AND
    /// lender's `ClaimedSeat.owner_kind == GlobalVault`. None for
    /// wallet lenders (the loader skips reading these accounts).
    pub vault_settle: Option<VaultSettleAccounts<'a, 'info>>,
}

/// Accounts needed to settle a vault-funded match in
/// `process_matched_loan`. Vault state aggregates update + 3-CPI atom
/// migration (vault.integration → global_vault_staging →
/// market_debt_vault → market.lender_integration).
#[allow(dead_code)] // load-bearing fields validated at load time
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

        // Validate marginfi_program (used as the bank's expected owner).
        require!(
            marginfi_program_ai.key == &marginfi_mocks::ID,
            YdeltaError::IncorrectAccount,
            "marginfi_program key does not match expected program id"
        )?;
        let debt_lending_pool: Pubkey = market.get_fixed()?.debt_lending_pool;
        let collateral_lending_pool: Pubkey = market.get_fixed()?.collateral_lending_pool;
        require!(
            *debt_bank_ai.key == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "debt_bank does not match market.debt_lending_pool"
        )?;
        let debt_bank = MarginfiBankInfo::new(debt_bank_ai, marginfi_program_ai.key)?;

        // Read the queue node up front so we can dispatch on SECONDARY
        // flags. The node is copied (Pod) so the loader can drop the
        // borrow before the processor mutates the market.
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

        let is_secondary =
            (queue_node.flags & crate::state::market::MATCHED_LOAN_FLAG_SECONDARY) != 0;
        let is_split =
            (queue_node.flags & crate::state::market::MATCHED_LOAN_FLAG_SECONDARY_SPLIT) != 0;

        // Branch on flags to determine the loan_a slot's expected role
        // (empty PDA to create, vs init PDA to mutate) and the rest of
        // the account-list shape.
        let mode = match (is_secondary, is_split) {
            (false, _) => ProcessMatchedLoanMode::Primary,
            (true, false) => ProcessMatchedLoanMode::SecondaryFull,
            (true, true) => ProcessMatchedLoanMode::SecondarySplit,
        };

        let (loan, existing_loan, loan_bump) = match mode {
            ProcessMatchedLoanMode::Primary => {
                // Empty Loan PDA at [b"loan", market, queue.sequence_le].
                let (expected_loan, bump) = crate::state::loan::loan_pda(market.key, sequence);
                require!(
                    *loan_ai.key == expected_loan,
                    YdeltaError::IncorrectAccount,
                    "loan account does not match per-(market, sequence) PDA"
                )?;
                (Some(EmptyAccount::new(loan_ai)?), None, bump)
            }
            ProcessMatchedLoanMode::SecondaryFull | ProcessMatchedLoanMode::SecondarySplit => {
                // Existing Loan PDA at the queue node's
                // referenced_loan_sequence.
                let referenced_seq = queue_node.referenced_loan_sequence;
                let (expected_loan, _bump) =
                    crate::state::loan::loan_pda(market.key, referenced_seq);
                require!(
                    *loan_ai.key == expected_loan,
                    YdeltaError::IncorrectAccount,
                    "loan_a does not match the referenced loan PDA \
                     [b\"loan\", market, referenced_loan_sequence={}]",
                    referenced_seq
                )?;
                let existing = YdeltaAccountInfo::<crate::state::loan::LoanFixed>::new(loan_ai)?;
                (None, Some(existing), 0u8)
            }
        };

        // SECONDARY paths add: collateral_bank, debt_oracle_ais,
        // collateral_oracle_ais. SECONDARY_SPLIT also adds
        // split_loan_to_create. Oracle slices are variadic per the
        // bank's `OracleSetup`; pubkey-pinned per-slot by
        // `MarginfiOracleAis::load`.
        let (collateral_bank, debt_oracle_ais, collateral_oracle_ais, split_loan_to_create) =
            match mode {
                ProcessMatchedLoanMode::Primary => (None, None, None, None),
                ProcessMatchedLoanMode::SecondaryFull | ProcessMatchedLoanMode::SecondarySplit => {
                    let collateral_bank_ai = next_account_info(account_iter)?;
                    require!(
                        *collateral_bank_ai.key == collateral_lending_pool,
                        YdeltaError::IncorrectAccount,
                        "collateral_bank does not match market.collateral_lending_pool"
                    )?;
                    let collateral_bank =
                        MarginfiBankInfo::new(collateral_bank_ai, marginfi_program_ai.key)?;

                    let debt_oracle_ais = MarginfiOracleAis::load(account_iter, debt_bank_ai)?;
                    let collateral_oracle_ais =
                        MarginfiOracleAis::load(account_iter, collateral_bank_ai)?;

                    let split_loan_to_create = if mode == ProcessMatchedLoanMode::SecondarySplit {
                        let split_ai = next_account_info(account_iter)?;
                        // Derive the split PDA from the QUEUE NODE'S
                        // stable `sequence` (== the cranker's
                        // `params.matched_loan_sequence` ix data,
                        // which is also the key the loader uses to
                        // look up the queue node), under the distinct
                        // `SPLIT_LOAN_SEED` prefix. Stable across the
                        // slot — concurrent place_orders bumping
                        // `market.matched_loan_sequence` cannot
                        // invalidate the cranker's pre-derived PDA.
                        let (expected_split, _bump) =
                            crate::state::loan::split_loan_pda(market.key, sequence);
                        require!(
                            *split_ai.key == expected_split,
                            YdeltaError::IncorrectAccount,
                            "split_loan_to_create PDA must equal \
                             [b\"split_loan\", market, queue_node_sequence={}]",
                            sequence
                        )?;
                        Some(EmptyAccount::new(split_ai)?)
                    } else {
                        None
                    };

                    (
                        Some(collateral_bank),
                        Some(debt_oracle_ais),
                        Some(collateral_oracle_ais),
                        split_loan_to_create,
                    )
                }
            };

        // Vault settlement accounts. Required when:
        // - Primary mode: the lender's seat is risk-profile-owned (the
        //   cranker's `do_vault_settle` migrates atoms vault → market).
        // - Secondary mode: the NEW lender's seat is risk-profile-owned
        //   (a SecondaryLoanSale bid crossed a risk-profile maker, so
        //   the cranker migrates the buyer's cash atoms vault → market
        //   for the seller's payout).
        let vault_settle: Option<VaultSettleAccounts<'a, 'info>> = {
            let trigger_seat_index: hypertree::DataIndex = match mode {
                ProcessMatchedLoanMode::Primary => queue_node.lender_seat_index,
                ProcessMatchedLoanMode::SecondaryFull | ProcessMatchedLoanMode::SecondarySplit => {
                    queue_node.new_lender_seat_index
                }
            };
            if trigger_seat_index == NIL {
                None
            } else {
                let owner_kind: u8 = {
                    let market_data = market.info.try_borrow_data()?;
                    let dynamic = &market_data[std::mem::size_of::<MarketFixed>()..];
                    crate::state::market::get_helper_seat(dynamic, trigger_seat_index)
                        .get_value()
                        .owner_kind
                };
                if owner_kind == crate::state::OWNER_KIND_RISK_PROFILE {
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
            existing_loan,
            split_loan_to_create,
            loan_bump,
            debt_bank,
            collateral_bank,
            debt_oracle_ais,
            collateral_oracle_ais,
            system_program,
            mode,
            queue_node,
            queue_node_index,
            vault_settle,
        })
    }
}

/// Read the trailing vault-settlement accounts on a vault-funded
/// primary match. Account order on the wire:
/// 1. vault PDA, 2. global_vault_signer, 3. global_vault_staging, 4.
/// vault.integration_account, 5. market.debt_vault (staging),
/// 6. market.lender_marginfi_account, 7. market_signer,
/// 8. liquidity_vault, 9. bank_liquidity_vault_authority,
/// 10. bank_oracle, 11. mint, 12. token_program.
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
    let vault_key = *vault_ai.key;

    // PDA validation.
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

    // Pin the vault to the canonical PDA derived from the loan's debt
    // mint. yDelta's create_vault enforces one vault per mint, so this
    // is equivalent to "the only vault that can lend this mint" — no
    // way for a malicious cranker to swap in a same-mint impostor.
    let (expected_global_vault_pda, _) = crate::state::vault::global_vault_pda(&mint_key);
    require!(
        vault_key == expected_global_vault_pda,
        YdeltaError::IncorrectAccount,
        "vault PDA does not match expected derivation from loan.debt_mint"
    )?;

    // Vault.mint must equal the loan's debt mint.
    {
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
    }

    // Market debt vault.
    let (expected_market_debt_vault, _) =
        crate::validation::get_vault_address(market_key, &mint_key);
    require!(
        *market_debt_vault_ai.key == expected_market_debt_vault,
        YdeltaError::IncorrectAccount,
        "market_debt_vault PDA mismatch"
    )?;

    // Market signer + lender integration account.
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

    // Bank liquidity vault + LVA + oracle.
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

    // Bind authorities to the respective signers.
    let global_vault_integration_account = MarginfiAccountInfo::new_with_expected_authority(
        vault_integration_ai,
        marginfi_program_id,
        vault_signer_ai.key,
    )?;
    let market_lender_integration_account = MarginfiAccountInfo::new_with_expected_authority(
        market_lender_integration_ai,
        marginfi_program_id,
        &expected_market_signer,
    )?;

    require!(
        marginfi_program_ai.key == marginfi_program_id,
        YdeltaError::IncorrectAccount,
        "vault settle: marginfi_program account mismatch"
    )?;
    let marginfi_program = MarginfiProgram::new(marginfi_program_ai)?;
    let marginfi_group = MarginfiGroupInfo::new(marginfi_group_ai, marginfi_program_id)?;

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

/// Account list for `ClaimRepayment`. Lender signs. Pure bookkeeping —
/// no SPL transfer, no marginfi CPI. The atoms behind the lender's
/// claim are already sitting on the market's marginfi-account asset
/// position (deposited there by `process_repay`'s
/// `marginfi.deposit`); the claim just converts them to seat-side
/// shares the lender can `process_withdraw` later.
///
/// `cranker_refund`: optional trailing account. When the claim closes
/// the loan PDA (full settlement), ydelta zeros the discriminator and
/// transfers the rent lamports to this account if it was passed AND its
/// key matches `loan.created_by`. If absent, the lamports stay
/// stranded on the (now-zeroed) loan address until a separate sweeper
/// ix recovers them.
pub(crate) struct ClaimRepaymentContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub loan: YdeltaAccountInfo<'a, 'info, crate::state::loan::LoanFixed>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    /// Signer's UserAccount mirror.
    pub user_account_ai: &'a AccountInfo<'info>,
    pub cranker_refund: Option<&'a AccountInfo<'info>>,
}

impl<'a, 'info> ClaimRepaymentContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let loan = YdeltaAccountInfo::<crate::state::loan::LoanFixed>::new(next_account_info(
            account_iter,
        )?)?;
        let debt_bank_ai = next_account_info(account_iter)?;
        let marginfi_program_ai = next_account_info(account_iter)?;
        // Signer's UserAccount + system_program (mandatory).
        let user_account_ai = next_account_info(account_iter)?;
        let system_program_ai = next_account_info(account_iter)?;
        let _ = crate::validation::user_account::ensure_user_account_for_signer(
            &payer,
            user_account_ai,
            system_program_ai,
        )?;
        // Trailing optional cranker_refund account.
        let cranker_refund = account_iter.next();

        // PDA + market binding.
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

        require!(
            marginfi_program_ai.key == &marginfi_mocks::ID,
            YdeltaError::IncorrectAccount,
            "marginfi_program key does not match expected program id"
        )?;
        let debt_lending_pool: Pubkey = market.get_fixed()?.debt_lending_pool;
        require!(
            *debt_bank_ai.key == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "debt_bank does not match market.debt_lending_pool"
        )?;
        let debt_bank = MarginfiBankInfo::new(debt_bank_ai, marginfi_program_ai.key)?;

        Ok(Self {
            payer,
            market,
            loan,
            debt_bank,
            user_account_ai,
            cranker_refund,
        })
    }
}

/// Account list for `ClaimRepaymentForRiskProfile`. Permissionless
/// cranker. Realises a fully-repaid risk-profile loan: drains the
/// lender's claim from `market.lender_integration_account` back into
/// the GlobalVault's marginfi account via a withdraw → SPL transfer →
/// deposit hop, and closes the loan PDA.
#[allow(dead_code)]
pub(crate) struct ClaimRepaymentForRiskProfileContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub loan: YdeltaAccountInfo<'a, 'info, crate::state::loan::LoanFixed>,
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
    /// Variadic oracle slice for `debt_bank`. See [`MarginfiOracleAis`].
    pub debt_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub debt_mint: MintAccountInfo<'a, 'info>,
    pub token_program: TokenProgram<'a, 'info>,
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
    pub cranker_refund: Option<&'a AccountInfo<'info>>,
}

impl<'a, 'info> ClaimRepaymentForRiskProfileContext<'a, 'info> {
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
        require!(
            loan.get_fixed()?.lender_kind == crate::state::OWNER_KIND_RISK_PROFILE,
            YdeltaError::InvalidArgument,
            "claim_repayment_for_risk_profile: loan is not risk-profile-funded \
             (use claim_repayment for wallet lenders)"
        )?;

        let market_fixed: Ref<MarketFixed> = market.get_fixed()?;
        let debt_mint_pk: Pubkey = market_fixed.debt_mint;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let lender_integration_pk: Pubkey = market_fixed.lender_integration_account;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
        drop(market_fixed);

        let global_vault_ai = next_account_info(account_iter)?;
        let global_vault =
            YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(global_vault_ai)?;
        require!(
            *global_vault.info.key == loan.get_fixed()?.lender_global_vault,
            YdeltaError::IncorrectAccount,
            "global_vault does not match loan.lender_global_vault"
        )?;
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
        // Variadic oracle slice for debt_bank — count from the bank's
        // `OracleSetup`, pubkey-pinned per-slot.
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
        let debt_bank = MarginfiBankInfo::new(debt_bank_ai, marginfi_program.info.key)?;
        // Bind authority to market_signer.
        let lender_marginfi_account = MarginfiAccountInfo::new_with_expected_authority(
            lender_mfi_ai,
            marginfi_program.info.key,
            &market_signer_pk,
        )?;
        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;

        // Bank liquidity-vault binding. Oracle pubkeys pinned by
        // `MarginfiOracleAis::load`.
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
        // Bind authority to global_vault_signer.
        let global_vault_integration_account = MarginfiAccountInfo::new_with_expected_authority(
            global_vault_integration_ai,
            marginfi_program.info.key,
            global_vault_signer_ai.key,
        )?;

        let cranker_refund = account_iter.next();

        Ok(Self {
            payer,
            market,
            loan,
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
            cranker_refund,
        })
    }
}

/// Account list for `Repay`. Borrower signs. Atoms hop borrower wallet
/// → market staging vault → `market.lender_integration_account` via
/// marginfi.deposit. Same flow regardless of lender kind; the lender
/// drains via `claim_repayment` (wallet) or
/// `claim_repayment_for_risk_profile` (risk_profile-funded).
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
    /// Loaded for symmetry with the place_order account list and as
    /// future-compat for ixs that need the live collateral-bank read
    /// during repay. Currently unused: full-repay credits collateral
    /// back at the loan's place-time snapshot, not the live bank
    /// value.
    #[allow(dead_code)]
    pub collateral_bank: MarginfiBankInfo<'a, 'info>,
    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
    pub user_account_ai: &'a AccountInfo<'info>,
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

        // Verify the loan PDA matches the on-disk sequence.
        let market_key = *market.key;
        let loan_sequence: u64 = loan.get_fixed()?.matched_loan_sequence;
        let (expected_loan, _bump) = crate::state::loan::loan_pda(&market_key, loan_sequence);
        require!(
            *loan.info.key == expected_loan,
            YdeltaError::IncorrectAccount,
            "loan PDA does not match [b\"loan\", market, sequence={}]",
            loan_sequence
        )?;
        // Loan must belong to this market.
        require!(
            loan.get_fixed()?.market == market_key,
            YdeltaError::IncorrectAccount,
            "loan.market does not match passed-in market account"
        )?;

        let market_fixed: Ref<MarketFixed> = market.get_fixed()?;
        let debt_mint_pk: Pubkey = market_fixed.debt_mint;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let collateral_lending_pool: Pubkey = market_fixed.collateral_lending_pool;
        // Repay routes to the **lender-side** marginfi-account by
        // default. Wallet-vs-wallet semantics: atoms top up A's USDC
        // asset (which the lender then claims). The P2Pool branch
        // needs both lender and borrower accounts to (a) reduce B's
        // liability via marginfi.repay, then (b) credit A so the
        // lender can claim.
        let lender_integration_account: Pubkey = market_fixed.lender_integration_account;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
        drop(market_fixed);

        let borrower_token = TokenAccountInfo::new_with_owner(
            next_account_info(account_iter)?,
            &debt_mint_pk,
            payer.key,
        )?;

        // Per-market staging vault for the debt mint.
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

        // Integration accounts (lender side).
        let group_ai = next_account_info(account_iter)?;
        let mfi_acct_ai = next_account_info(account_iter)?;
        let debt_bank_ai = next_account_info(account_iter)?;
        let liquidity_vault_ai = next_account_info(account_iter)?;
        let collateral_bank_ai = next_account_info(account_iter)?;
        let market_signer_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;

        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        let marginfi_account = MarginfiAccountInfo::new(mfi_acct_ai, marginfi_program.info.key)?;
        let debt_bank = MarginfiBankInfo::new(debt_bank_ai, marginfi_program.info.key)?;
        let collateral_bank = MarginfiBankInfo::new(collateral_bank_ai, marginfi_program.info.key)?;

        require!(
            *mfi_acct_ai.key == lender_integration_account,
            YdeltaError::IncorrectAccount,
            "marginfi_account does not match market.lender_integration_account \
             (repay routes to lender side for wallet-vs-wallet path)"
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
        let _ = crate::validation::user_account::ensure_user_account_for_signer(
            &payer,
            user_account_ai,
            system_program_ai,
        )?;

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
            market_signer: market_signer_ai,
            market_signer_bump,
            marginfi_program,
            user_account_ai,
        })
    }
}

// ─────────────────── Liquidation ───────────────────

/// Account list for `SettleMaturedLoan` and `LiquidateLoan`. Permissionless
/// keeper. Liquidator pays up to `repay_atoms_max` of the loan's
/// `outstanding_debt_atoms` in debt-mint and receives pro-rata
/// collateral. Atoms hop liquidator wallet → market debt staging →
/// `market.lender_integration_account` regardless of lender kind; the
/// lender drains via `claim_repayment` (wallet) or
/// `claim_repayment_for_risk_profile` (risk_profile-funded).
pub(crate) struct SettleMaturedLoanContext<'a, 'info> {
    pub payer: Signer<'a, 'info>, // liquidator
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
    /// Variadic oracle slice for `debt_bank`. See [`MarginfiOracleAis`].
    pub debt_oracle_ais: MarginfiOracleAis<'a, 'info>,
    /// Variadic oracle slice for `collateral_bank`. See [`MarginfiOracleAis`].
    pub collateral_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub debt_mint: MintAccountInfo<'a, 'info>,
    pub collateral_mint: MintAccountInfo<'a, 'info>,
    pub token_program: TokenProgram<'a, 'info>,
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
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
        // Variadic oracle slices for debt + collateral banks. Pubkey-pinned
        // per-slot against `bank.config.oracle_keys[i]`.
        let debt_oracle_ais = MarginfiOracleAis::load(account_iter, debt_bank_ai)?;
        let collateral_oracle_ais = MarginfiOracleAis::load(account_iter, collateral_bank_ai)?;
        let debt_mint_ai = next_account_info(account_iter)?;
        let collateral_mint_ai = next_account_info(account_iter)?;
        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let group_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;

        // Loan PDA validation.
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
        require!(
            loan.get_fixed()?.outstanding_debt_atoms > 0,
            YdeltaError::InvalidArgument,
            "loan already fully repaid (outstanding == 0)"
        )?;

        // Market integration accounts + signer.
        let market_fixed: Ref<MarketFixed> = market.get_fixed()?;
        let debt_mint_pk: Pubkey = market_fixed.debt_mint;
        let collateral_mint_pk: Pubkey = market_fixed.collateral_mint;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let collateral_lending_pool: Pubkey = market_fixed.collateral_lending_pool;
        let lender_integration_pk: Pubkey = market_fixed.lender_integration_account;
        let borrower_integration_pk: Pubkey = market_fixed.borrower_integration_account;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
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

        // Bind lender + borrower marginfi-account authorities to the
        // per-market signer PDA. PDA-key checks elsewhere are the
        // primary defense; the on-disk authority bind closes the gap
        // should those address checks ever drift.
        let lender_marginfi_account = MarginfiAccountInfo::new_with_expected_authority(
            lender_mfi_ai,
            marginfi_program.info.key,
            &market_signer_pk,
        )?;
        let borrower_marginfi_account = MarginfiAccountInfo::new_with_expected_authority(
            borrower_mfi_ai,
            marginfi_program.info.key,
            &market_signer_pk,
        )?;
        let debt_bank = MarginfiBankInfo::new(debt_bank_ai, marginfi_program.info.key)?;
        let collateral_bank = MarginfiBankInfo::new(collateral_bank_ai, marginfi_program.info.key)?;
        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;

        // Liquidity-vault binding + LVA. Oracle pubkeys pinned by
        // `MarginfiOracleAis::load`.
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
        })
    }
}

/// Account list for `ProtocolFeeClaim`.
#[allow(dead_code)] // load-bearing fields validated at load time
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
    /// Variadic oracle slice for `debt_bank`. See [`MarginfiOracleAis`].
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
        // Variadic oracle slice for debt_bank — count from `OracleSetup`,
        // pubkey-pinned per-slot against `bank.config.oracle_keys[i]`.
        let debt_oracle_ais = MarginfiOracleAis::load(account_iter, debt_bank_ai)?;
        let debt_mint_ai = next_account_info(account_iter)?;
        let token_program = TokenProgram::new(next_account_info(account_iter)?)?;
        let group_ai = next_account_info(account_iter)?;
        let marginfi_program = MarginfiProgram::new(next_account_info(account_iter)?)?;

        // Protocol-admin gate. These are the protocol's fees (not the
        // market's), so the recipient must be the global protocol admin
        // — gating on `market.admin` would let every per-market admin
        // siphon protocol fees from their own market.
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

        // Bank-derived validation: liquidity_vault. Oracle pubkeys
        // pinned by `MarginfiOracleAis::load`.
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

        // Bind `admin_debt_token` to the protocol_admin's ownership,
        // not just to the debt mint. Without the owner check the
        // admin can fat-finger fees into a third-party ATA (or the
        // admin signer's compromised key into an attacker ATA) —
        // both unrecoverable. The owner check makes the failure mode
        // "tx fails until the admin builds the right account list"
        // instead.
        let admin_debt_token =
            TokenAccountInfo::new_with_owner(admin_debt_token_ai, &debt_mint_pk, payer.info.key)?;
        let market_debt_vault = TokenAccountInfo::new(market_debt_vault_ai, &debt_mint_pk)?;
        let debt_liquidity_vault = TokenAccountInfo::new(debt_liquidity_vault_ai, &debt_mint_pk)?;
        let debt_mint = MintAccountInfo::new(debt_mint_ai)?;
        let debt_bank = MarginfiBankInfo::new(debt_bank_ai, marginfi_program.info.key)?;
        // Bind lender marginfi-account authority to market_signer.
        let lender_marginfi_account = MarginfiAccountInfo::new_with_expected_authority(
            lender_mfi_ai,
            marginfi_program.info.key,
            &market_signer_pk,
        )?;
        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;

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

// ─────────────────── `GlobalVault` bootstrap loaders ───────────────────

/// Account list for `CreateVault`. The vault PDA itself is uninitialised
/// at this point (lamports == 0) — the processor allocates it via
/// `system_instruction::create_account` signed by the vault seeds.
/// The marginfi `integration_account` is also uninit; the processor
/// fires `marginfi_account_initialize` signed by the integration-account
/// seeds + the global_vault_signer seeds. The per-vault staging SPL token
/// account at `[b"global_vault_staging", vault]` is allocated as part of
/// the same ix (owned by `global_vault_signer`).
pub(crate) struct CreateVaultContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    /// Uninit at load time; allocated by the processor.
    pub vault: &'a AccountInfo<'info>,
    pub vault_bump: u8,
    pub mint: MintAccountInfo<'a, 'info>,
    /// Read-only — used as the authority on the marginfi CPI.
    pub global_vault_signer: &'a AccountInfo<'info>,
    pub global_vault_signer_bump: u8,
    /// Uninit at load time; allocated by the marginfi CPI.
    pub integration_account: &'a AccountInfo<'info>,
    pub integration_account_bump: u8,
    /// Uninit at load time; allocated by the processor as an SPL token
    /// account owned by `global_vault_signer`.
    pub global_vault_staging: &'a AccountInfo<'info>,
    pub global_vault_staging_bump: u8,
    pub token_program: TokenProgram<'a, 'info>,
    pub token_program_22: TokenProgram<'a, 'info>,
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    /// The marginfi bank for the vault's mint. Pinned at create
    /// time — depositors / withdrawers always route through this bank.
    pub lending_pool: MarginfiBankInfo<'a, 'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
    pub system_program: Program<'a, 'info>,
}

impl<'a, 'info> CreateVaultContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let global_config = load_global_config(account_iter)?;
        require!(
            *payer.info.key == global_config.get_fixed()?.protocol_admin,
            YdeltaError::ProtocolAdminRequired,
            "create_vault: signer must equal GlobalConfig.protocol_admin"
        )?;
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

        // PDA derivation + validation.
        let (expected_vault, vault_bump) = crate::state::vault::global_vault_pda(mint.info.key);
        require!(
            *vault_ai.key == expected_vault,
            YdeltaError::IncorrectAccount,
            "vault does not match [b\"vault\", mint]"
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

        // Vault must not exist yet (one-shot creation, mint-uniqueness invariant).
        require!(
            vault_ai.lamports() == 0,
            YdeltaError::IncorrectAccount,
            "vault PDA already exists; one GlobalVault per mint"
        )?;
        require!(
            integration_account_ai.lamports() == 0,
            YdeltaError::IncorrectAccount,
            "integration_account already exists; one per vault"
        )?;
        require!(
            vault_staging_ai.lamports() == 0,
            YdeltaError::IncorrectAccount,
            "global_vault_staging already exists; one per vault"
        )?;

        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        let lending_pool = MarginfiBankInfo::new(lending_pool_ai, marginfi_program.info.key)?;

        // The bank must serve the vault's mint.
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

/// Account list for `GlobalVaultDeposit`. Depositor signs; atoms hop
/// `depositor_token → global_vault_staging → integration_account` (the
/// vault's marginfi account, into the pinned `lending_pool` bank).
/// Then the depositor's `UserAccount.vault_positions` mirror is
/// upserted with the freshly minted shares.
pub(crate) struct GlobalVaultDepositContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
    pub mint: MintAccountInfo<'a, 'info>,
    /// Authority of `global_vault_staging` and the marginfi `integration_account`.
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

        // ─── PDA + cross-account validation ───
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
        drop(vault_fixed);

        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        let integration_account =
            MarginfiAccountInfo::new(integration_ai, marginfi_program.info.key)?;
        let lending_pool = MarginfiBankInfo::new(lending_pool_ai, marginfi_program.info.key)?;
        let global_vault_staging = TokenAccountInfo::new_with_owner_and_key(
            vault_staging_ai,
            &mint_key,
            vault_signer_ai.key,
            &expected_staging,
        )?;
        let depositor_token =
            TokenAccountInfo::new_with_owner(depositor_token_ai, &mint_key, payer.info.key)?;

        // Liquidity-vault validation.
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

        // Auto-create the depositor's UserAccount on first deposit.
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

/// Account list for `GlobalVaultWithdraw`. Depositor signs and burns
/// shares; atoms hop `integration_account → global_vault_staging → depositor`.
/// Marginfi's withdraw runs a health check that needs the bank +
/// oracle (vault's only active balance is the lending_pool, so a
/// single (bank, oracle) pair suffices).
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
        drop(vault_fixed);

        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;
        let integration_account =
            MarginfiAccountInfo::new(integration_ai, marginfi_program.info.key)?;
        let lending_pool = MarginfiBankInfo::new(lending_pool_ai, marginfi_program.info.key)?;
        let global_vault_staging = TokenAccountInfo::new_with_owner_and_key(
            vault_staging_ai,
            &mint_key,
            vault_signer_ai.key,
            &expected_staging,
        )?;
        let depositor_token =
            TokenAccountInfo::new_with_owner(depositor_token_ai, &mint_key, payer.info.key)?;

        // Validate liquidity_vault matches the bank's record + bank's
        // configured oracle.
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

        // Validate bank's liquidity-vault authority PDA.
        let (expected_lva, _) = Pubkey::find_program_address(
            &[b"liquidity_vault_auth", lending_pool_ai.key.as_ref()],
            marginfi_program.info.key,
        );
        require!(
            *bank_liquidity_vault_authority_ai.key == expected_lva,
            YdeltaError::IncorrectAccount,
            "bank_liquidity_vault_authority does not match marginfi PDA"
        )?;

        // Auto-create the depositor's UserAccount on first interaction.
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

/// Account list for `ClaimCuratorFee`. Curator-gated.
/// Withdraws `accumulated_curator_fee_atoms` from the vault's
/// integration_account out to the curator's wallet via marginfi.withdraw +
/// SPL transfer.
#[allow(dead_code)] // load-bearing fields validated at load time
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
        let debt_bank = MarginfiBankInfo::new(debt_bank_ai, marginfi_program.info.key)?;

        // Bank vault + LVA + oracle.
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
            MarginfiAccountInfo::new(vault_integration_ai, marginfi_program.info.key)?;

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

/// Account list for `ClaimSeatForRiskProfile`.
pub(crate) struct ClaimSeatForRiskProfileContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub system_program: Program<'a, 'info>,
}

impl<'a, 'info> ClaimSeatForRiskProfileContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let system_program = Program::new(next_account_info(account_iter)?, &system_program::id())?;

        // Admin gate.
        let global_vault_admin: Pubkey = vault.get_fixed()?.global_vault_admin;
        require!(
            *payer.info.key == global_vault_admin,
            YdeltaError::VaultAdminRequired,
            "claim_seat_for_risk_profile: signer is not vault.global_vault_admin"
        )?;

        // Mint binding. Without this check, a foreign-mint vault could
        // claim a seat on this market and place orders that the
        // borrower-side place_order would settle against the market's
        // canonical debt-mint vault — black-holing liquidity behind
        // loans whose lender_global_vault is on a different mint and can no
        // longer settle.
        require_vault_mint_matches_market(&vault, &market)?;

        Ok(Self {
            payer,
            vault,
            market,
            system_program,
        })
    }
}

/// Account list for `CancelOrderForRiskProfile`. Curator-gated.
pub(crate) struct CancelOrderForRiskProfileContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub _system_program: Program<'a, 'info>,
}

impl<'a, 'info> CancelOrderForRiskProfileContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        require_market_not_paused(&market)?;
        let _system_program =
            Program::new(next_account_info(account_iter)?, &system_program::id())?;

        // Mint binding (also covers place_order_for_risk_profile, which
        // reuses this loader). Prevents a foreign-mint vault from
        // operating on a market whose debt mint it doesn't match.
        require_vault_mint_matches_market(&vault, &market)?;

        Ok(Self {
            payer,
            vault,
            market,
            _system_program,
        })
    }
}

/// Account list for `CreateRiskProfile`.
pub(crate) struct CreateRiskProfileContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
    pub _system_program: Program<'a, 'info>,
}

impl<'a, 'info> CreateRiskProfileContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();

        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        let _system_program =
            Program::new(next_account_info(account_iter)?, &system_program::id())?;

        // Admin gate.
        let global_vault_admin: Pubkey = vault.get_fixed()?.global_vault_admin;
        require!(
            *payer.info.key == global_vault_admin,
            YdeltaError::VaultAdminRequired,
            "create_risk_profile: signer ({}) is not global_vault_admin ({})",
            payer.info.key,
            global_vault_admin
        )?;

        Ok(Self {
            payer,
            vault,
            _system_program,
        })
    }
}

// ─────────────────── Admin transfers ───────────────────

/// Initiate `MarketFixed.admin` transfer. Signer must equal the current
/// admin (validated in the processor; loader just enforces shape).
pub(crate) struct TransferMarketAdminContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
}

impl<'a, 'info> TransferMarketAdminContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        // Admin-recovery exempt: TransferMarketAdmin / AcceptMarketAdmin
        // bypass the global pause so a paused-by-mistake state can be
        // rotated.
        let _ = load_global_config_no_pause(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        Ok(Self { payer, market })
    }
}

/// Finalize `MarketFixed.admin` transfer. Signer must equal pending_admin.
pub(crate) struct AcceptMarketAdminContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
}

impl<'a, 'info> AcceptMarketAdminContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        // Admin-recovery exempt: TransferMarketAdmin / AcceptMarketAdmin
        // bypass the global pause so a paused-by-mistake state can be
        // rotated.
        let _ = load_global_config_no_pause(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        Ok(Self { payer, market })
    }
}

/// Initiate `GlobalVaultFixed.global_vault_admin` transfer.
pub(crate) struct TransferGlobalVaultAdminContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
}

impl<'a, 'info> TransferGlobalVaultAdminContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        // Admin-recovery exempt: vault-admin / curator transfers
        // bypass the global pause so compromised keys can rotate
        // even when the protocol is paused.
        let _ = load_global_config_no_pause(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        Ok(Self { payer, vault })
    }
}

/// Finalize `GlobalVaultFixed.global_vault_admin` transfer.
pub(crate) struct AcceptGlobalVaultAdminContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
}

impl<'a, 'info> AcceptGlobalVaultAdminContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        // Admin-recovery exempt: vault-admin / curator transfers
        // bypass the global pause so compromised keys can rotate
        // even when the protocol is paused.
        let _ = load_global_config_no_pause(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        Ok(Self { payer, vault })
    }
}

/// Initiate per-profile `RiskProfile.curator` transfer.
pub(crate) struct TransferCuratorContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
}

impl<'a, 'info> TransferCuratorContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        // Admin-recovery exempt: vault-admin / curator transfers
        // bypass the global pause so compromised keys can rotate
        // even when the protocol is paused.
        let _ = load_global_config_no_pause(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        Ok(Self { payer, vault })
    }
}

/// Finalize per-profile `RiskProfile.curator` transfer.
pub(crate) struct AcceptCuratorContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
}

impl<'a, 'info> AcceptCuratorContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        // Admin-recovery exempt: vault-admin / curator transfers
        // bypass the global pause so compromised keys can rotate
        // even when the protocol is paused.
        let _ = load_global_config_no_pause(account_iter)?;
        let vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        Ok(Self { payer, vault })
    }
}

// ─────────────────── Market pause ───────────────────

/// Account list for `SetMarketPause`.
pub(crate) struct SetMarketPauseContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
}

impl<'a, 'info> SetMarketPauseContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        // Slot-1 GlobalConfig conformance with the rest of the
        // state-mutating loaders. Use the `_no_pause` variant since
        // SetMarketPause is part of the admin-recovery surface — it
        // must remain callable while the protocol is globally paused.
        let _ = load_global_config_no_pause(account_iter)?;
        let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;
        Ok(Self { payer, market })
    }
}

/// Account list for `SyncMarketSeatsForRiskProfile`. Permissionless
/// — `payer` may be any signer. The loader collects the (≤8) trailing
/// market accounts; the processor cross-checks each against
/// `RiskProfile.active_markets` and re-stamps the cached
/// `risk_profile_max_ltv_bps` on every present `(global_vault, OWNER_KIND_RISK_PROFILE,
/// profile_id)` seat.
#[allow(dead_code)] // load-bearing: validated at load time
pub(crate) struct SyncMarketSeatsForRiskProfileContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub global_vault: YdeltaAccountInfo<'a, 'info, crate::state::vault::GlobalVaultFixed>,
    pub markets: Vec<&'a AccountInfo<'info>>,
}

impl<'a, 'info> SyncMarketSeatsForRiskProfileContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let _ = load_global_config(account_iter)?;
        let global_vault = YdeltaAccountInfo::<crate::state::vault::GlobalVaultFixed>::new(
            next_account_info(account_iter)?,
        )?;
        // Trailing variable-length list of market accounts. Each must
        // be ydelta-owned + carry the market discriminator. Cap at the
        // active_markets max so a stranger can't overflow the call.
        const MAX: usize = crate::state::vault::RiskProfile::ACTIVE_MARKETS_MAX as usize;
        let mut markets: Vec<&'a AccountInfo<'info>> = Vec::with_capacity(MAX);
        for ai in account_iter.by_ref() {
            require!(
                markets.len() < MAX,
                YdeltaError::InvalidArgument,
                "sync_market_seats_for_risk_profile: more than {} market accounts passed",
                MAX
            )?;
            // Validate the discriminator/owner so a stranger can't pass
            // arbitrary yDelta accounts as "markets".
            let _ = YdeltaAccountInfo::<MarketFixed>::new(ai)?;
            markets.push(ai);
        }
        Ok(Self {
            payer,
            global_vault,
            markets,
        })
    }
}

/// Helper used by every state-mutating market ix's loader: rejects
/// when the market is paused.
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

/// Bind a vault account to a specific market by requiring
/// `vault.mint == market.debt_mint`. yDelta enforces one vault per
/// mint via `create_vault`'s PDA derivation, so a vault on a
/// different mint is by definition a different vault. Without this
/// check, a foreign-mint vault could claim seats / place orders on a
/// market whose canonical vault is elsewhere — borrower-side
/// place_order would route deltas to the canonical vault while
/// `loan.lender_global_vault` points at the foreign one, locking liquidity.
pub(crate) fn require_vault_mint_matches_market(
    vault: &YdeltaAccountInfo<'_, '_, crate::state::vault::GlobalVaultFixed>,
    market: &YdeltaAccountInfo<'_, '_, MarketFixed>,
) -> Result<(), ProgramError> {
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

// ─────────────────── GlobalConfig + global pause ───────────────────

/// Account list for `CreateGlobalConfig`. One-shot. Signer becomes
/// the initial `protocol_admin` — gated against the program's BPF
/// upgrade authority so a deploy-time race can't let a front-runner
/// claim protocol_admin.
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

        // Bind the signer to the program's BPF upgrade authority.
        // Layout of `BpfLoaderUpgradeable::ProgramData`'s serialized
        // metadata (45 bytes total, bincode):
        //   4 bytes: enum tag (3 = ProgramData)
        //   8 bytes: slot (u64 LE)
        //   1 byte:  Option<Pubkey> tag (0 = None, 1 = Some)
        //   32 bytes: upgrade_authority pubkey (when tag == 1)
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

/// Initiate `GlobalConfig.protocol_admin` transfer.
pub(crate) struct TransferProtocolAdminContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub global_config: YdeltaAccountInfo<'a, 'info, crate::state::global_config::GlobalConfig>,
}

impl<'a, 'info> TransferProtocolAdminContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let global_config = YdeltaAccountInfo::<crate::state::global_config::GlobalConfig>::new(
            next_account_info(account_iter)?,
        )?;
        Ok(Self {
            payer,
            global_config,
        })
    }
}

/// Finalize `GlobalConfig.protocol_admin` transfer.
pub(crate) struct AcceptProtocolAdminContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub global_config: YdeltaAccountInfo<'a, 'info, crate::state::global_config::GlobalConfig>,
}

impl<'a, 'info> AcceptProtocolAdminContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let global_config = YdeltaAccountInfo::<crate::state::global_config::GlobalConfig>::new(
            next_account_info(account_iter)?,
        )?;
        Ok(Self {
            payer,
            global_config,
        })
    }
}

/// Set global pause. Protocol-admin-gated.
pub(crate) struct SetGlobalPauseContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub global_config: YdeltaAccountInfo<'a, 'info, crate::state::global_config::GlobalConfig>,
}

impl<'a, 'info> SetGlobalPauseContext<'a, 'info> {
    pub fn load(accounts: &'a [AccountInfo<'info>]) -> Result<Self, ProgramError> {
        let account_iter: &mut Iter<AccountInfo<'info>> = &mut accounts.iter();
        let payer = Signer::new_payer(next_account_info(account_iter)?)?;
        let global_config = YdeltaAccountInfo::<crate::state::global_config::GlobalConfig>::new(
            next_account_info(account_iter)?,
        )?;
        Ok(Self {
            payer,
            global_config,
        })
    }
}

/// Helper used by every state-mutating ix's loader: rejects when the
/// global pause flag is on. Idempotent — works even when the
/// `global_config` account is freshly initialized (post-`CreateGlobalConfig`).
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

/// Combined helper: pull the next account from `account_iter`, validate
/// it as the singleton `GlobalConfig` PDA, check the pause flag, and
/// return the typed wrapper. Every state-mutating ix's loader calls
/// this at the start.
pub(crate) fn load_global_config<'a, 'info>(
    account_iter: &mut Iter<'a, AccountInfo<'info>>,
) -> Result<YdeltaAccountInfo<'a, 'info, crate::state::global_config::GlobalConfig>, ProgramError> {
    let global_config = load_global_config_no_pause(account_iter)?;
    require_global_not_paused(&global_config)?;
    Ok(global_config)
}

/// Pause-exempt variant of `load_global_config`. Used by ixs that are
/// part of the admin-recovery surface (Market/Vault/Curator
/// transfer+accept) — the design intent is that a paused-by-mistake
/// state should still allow rotating compromised admin keys via the
/// two-step transfer flow. `TransferProtocolAdmin` and
/// `AcceptProtocolAdmin` are similarly exempt (they have their own
/// dedicated loader). `SetGlobalPause` itself is exempt for the same
/// recovery reason.
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

// ─────────────────── ConvertP2PoolToFixed ───────────────────

/// Account list for `ConvertP2PoolToFixed`. Borrower signs. The CPIs
/// are `marginfi.withdraw` on `lender_marginfi_account` followed by
/// `marginfi.repay` on `borrower_marginfi_account`, both signed by
/// `market_signer`. The ask-maker's seat is mutated in place to drop
/// their encumbered shares and mark the order's residual.
#[allow(dead_code)] // load-bearing fields validated at load time
pub(crate) struct ConvertP2PoolToFixedContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub loan: YdeltaAccountInfo<'a, 'info, crate::state::loan::LoanFixed>,
    pub borrower_marginfi_account: MarginfiAccountInfo<'a, 'info>,
    pub lender_marginfi_account: MarginfiAccountInfo<'a, 'info>,
    pub debt_bank: MarginfiBankInfo<'a, 'info>,
    pub debt_liquidity_vault: TokenAccountInfo<'a, 'info>,
    pub debt_bank_lva: &'a AccountInfo<'info>,
    /// Variadic oracle slice for `debt_bank`. See [`MarginfiOracleAis`].
    pub debt_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub collateral_bank: MarginfiBankInfo<'a, 'info>,
    /// Variadic oracle slice for `collateral_bank`. See [`MarginfiOracleAis`].
    pub collateral_oracle_ais: MarginfiOracleAis<'a, 'info>,
    pub market_debt_vault: TokenAccountInfo<'a, 'info>,
    pub debt_mint: MintAccountInfo<'a, 'info>,
    pub market_signer: &'a AccountInfo<'info>,
    pub market_signer_bump: u8,
    pub token_program: TokenProgram<'a, 'info>,
    pub marginfi_group: MarginfiGroupInfo<'a, 'info>,
    pub marginfi_program: MarginfiProgram<'a, 'info>,
    /// Optional refund target for the P2Pool loan PDA's lamports on
    /// full conversion. The processor closes the PDA when the loan is
    /// fully refinanced and refunds rent here iff the passed key
    /// matches `loan.created_by` (the cranker who originally promoted
    /// the P2Pool MatchedLoan). `None` skips the refund — lamports
    /// stay stranded on the now-zeroed account, recoverable by a
    /// future sweeper. Mirrors the cranker_refund pattern in
    /// `claim_repayment` and `claim_repayment_for_risk_profile`.
    pub cranker_refund: Option<&'a AccountInfo<'info>>,
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

        // Loan must be a P2Pool body on this market, still Active.
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
            // Verify the loan PDA address.
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
        let collateral_mint_pk: Pubkey = market_fixed.collateral_mint;
        let debt_lending_pool: Pubkey = market_fixed.debt_lending_pool;
        let collateral_lending_pool: Pubkey = market_fixed.collateral_lending_pool;
        let lender_integration_pk: Pubkey = market_fixed.lender_integration_account;
        let borrower_integration_pk: Pubkey = market_fixed.borrower_integration_account;
        let market_signer_pk: Pubkey = market_fixed.market_signer;
        let market_signer_bump: u8 = market_fixed.market_signer_bump;
        let market_debt_vault_pk: Pubkey = market_fixed.debt_vault;
        let _ = collateral_mint_pk;
        drop(market_fixed);

        let borrower_mfi_ai = next_account_info(account_iter)?;
        require!(
            *borrower_mfi_ai.key == borrower_integration_pk,
            YdeltaError::IncorrectAccount,
            "borrower_marginfi_account does not match per-market PDA"
        )?;
        let lender_mfi_ai = next_account_info(account_iter)?;
        require!(
            *lender_mfi_ai.key == lender_integration_pk,
            YdeltaError::IncorrectAccount,
            "lender_marginfi_account does not match per-market PDA"
        )?;

        let debt_bank_ai = next_account_info(account_iter)?;
        require!(
            *debt_bank_ai.key == debt_lending_pool,
            YdeltaError::IncorrectAccount,
            "debt_bank does not match market.debt_lending_pool"
        )?;
        let debt_liquidity_vault_ai = next_account_info(account_iter)?;
        let debt_bank_lva_ai = next_account_info(account_iter)?;
        // Variadic oracle slice for debt_bank — count read from
        // `BankConfigView::expected_oracle_account_count()`. Each entry
        // is pubkey-pinned against `bank.config.oracle_keys[i]`.
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

        // Bank-derived validation: liquidity_vault. Oracle keys are
        // pinned by `MarginfiOracleAis::load` above (per-slot match
        // against `bank.config.oracle_keys[i]`). Setup-count → multiple
        // oracles is allowed end-to-end; the decoder side will reject
        // any setup it doesn't yet know how to read.
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
        let debt_bank = MarginfiBankInfo::new(debt_bank_ai, marginfi_program.info.key)?;
        let collateral_bank = MarginfiBankInfo::new(collateral_bank_ai, marginfi_program.info.key)?;
        // Bind both marginfi-account authorities to market_signer.
        let borrower_marginfi_account = MarginfiAccountInfo::new_with_expected_authority(
            borrower_mfi_ai,
            marginfi_program.info.key,
            &market_signer_pk,
        )?;
        let lender_marginfi_account = MarginfiAccountInfo::new_with_expected_authority(
            lender_mfi_ai,
            marginfi_program.info.key,
            &market_signer_pk,
        )?;
        let marginfi_group = MarginfiGroupInfo::new(group_ai, marginfi_program.info.key)?;

        // Optional trailing `cranker_refund` account. Borrower (off-chain)
        // reads `loan.created_by` to know who paid the original PDA rent
        // and passes them here on a full-conversion refinance.
        let cranker_refund: Option<&'a AccountInfo<'info>> = account_iter.next();

        Ok(Self {
            payer,
            market,
            loan,
            borrower_marginfi_account,
            lender_marginfi_account,
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
            cranker_refund,
        })
    }
}

// ─────────────────── Liquidatability simulation gates ───────────────────
//
// `CheckLtvLiquidatable` and `CheckMaturityLiquidatable` are read-only
// gates designed for `simulateTransaction` callers (keepers, UIs).
// Each one wraps the SAME helper the real ix uses
// (`assert_ltv_breach` / `assert_past_grace_period` from
// `state/ltv.rs`), so a successful simulation guarantees the real ix
// would also pass that gate (modulo CPI side-effects).
//
// Account shape is a thin subset of `SettleMaturedLoanContext` — no
// liquidator token accounts, no staging vault, no SPL transfer
// plumbing. Just enough to read the loan body, the borrower's marginfi
// liability (for P2Pool live debt), and the oracle prices.

/// Account list for `CheckLtvLiquidatable` (tag 40).
#[allow(dead_code)] // load-bearing fields validated at load time
pub(crate) struct CheckLtvLiquidatableContext<'a, 'info> {
    pub payer: Signer<'a, 'info>,
    pub market: YdeltaAccountInfo<'a, 'info, MarketFixed>,
    pub loan: YdeltaAccountInfo<'a, 'info, crate::state::loan::LoanFixed>,
    /// Borrower-side marginfi-account at
    /// `MarketFixed.borrower_integration_account`. Only read for the
    /// P2Pool branch (`liability_shares × liability_share_value`); for
    /// Fixed loans it's read but the value is ignored (caller still
    /// passes the account for a uniform shape).
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
        let borrower_marginfi_account =
            MarginfiAccountInfo::new(borrower_mfi_ai, marginfi_program.info.key)?;
        let debt_bank = MarginfiBankInfo::new(debt_bank_ai, marginfi_program.info.key)?;
        let collateral_bank = MarginfiBankInfo::new(collateral_bank_ai, marginfi_program.info.key)?;

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

/// Account list for `CheckMaturityLiquidatable` (tag 41). No collateral
/// side — the maturity gate doesn't price the loan; it just checks the
/// time boundary and that live outstanding > 0.
#[allow(dead_code)]
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
        let borrower_marginfi_account =
            MarginfiAccountInfo::new(borrower_mfi_ai, marginfi_program.info.key)?;
        let debt_bank = MarginfiBankInfo::new(debt_bank_ai, marginfi_program.info.key)?;

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
