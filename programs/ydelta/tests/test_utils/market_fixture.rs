//! `MarketFixture` — a unified test fixture for end-to-end ydelta deposit /
//! withdraw / matching / loan-promotion flows that exercise the marginfi
//! adapter for real.
//!
//! Loads the same mainnet account dumps as `AdapterFixture` (group, USDC
//! bank/mint/vault/oracle, SOL bank/mint/vault/oracle) and on top creates a
//! ydelta market with `debt_mint = USDC`, `collateral_mint = wSOL`, banks
//! pointing at the corresponding mainnet banks. The market's
//! `marginfi_account` is the per-market PDA created at `create_market` time
//! by CPI-ing `marginfi_account_initialize`.
//!
//! SBF-only: ydelta's deposit/withdraw paths now CPI into `marginfi.so`,
//! so these tests can't run under native `cargo test`. Gate test modules
//! that use this fixture behind `#[cfg(feature = "test-sbf")]`.

use std::cell::{RefCell, RefMut};
use std::path::PathBuf;
use std::rc::Rc;

use solana_program::pubkey::Pubkey;
use solana_program_test::{ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::Account,
    instruction::Instruction,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

use ydelta::program::instruction_builders::{
    claim_curator_fee_instruction::claim_curator_fee_instruction,
    claim_repayment_instruction::claim_repayment_instruction,
    claim_seat_for_risk_profile_instruction::claim_seat_for_risk_profile_instruction,
    claim_seat_instruction::claim_seat_instruction,
    create_market_instructions::create_market_instructions,
    create_risk_profile_instruction::create_risk_profile_instruction,
    create_vault_instruction::create_vault_instruction,
    deposit_instruction::deposit_instruction,
    global_vault_deposit_instruction::global_vault_deposit_instruction,
    global_vault_withdraw_instruction::global_vault_withdraw_instruction,
    liquidate_loan_instruction::liquidate_loan_instruction,
    place_order_for_risk_profile_instruction::place_order_for_risk_profile_instruction,
    place_order_instruction::{place_order_instruction, secondary_place_order_instruction},
    process_matched_loan_instruction::{
        process_matched_loan_instruction, process_secondary_matched_loan_instruction,
        process_secondary_split_matched_loan_instruction,
    },
    repay_instruction::repay_instruction,
    update_order_instruction::update_secondary_order_instruction,
    withdraw_instruction::withdraw_instruction,
};
use ydelta::state::{OrderType, Side};

use super::marginfi_fixture::{load_account_from_fixture, mainnet};

pub struct MarketFixture {
    pub context: Rc<RefCell<ProgramTestContext>>,
    pub payer: Keypair,
    pub market: Keypair,
    /// Optional secondary market on the same `(USDC, wSOL)` pair, lazily
    /// stood up by `create_second_market()`. The vault is per-mint, so
    /// a single risk_profile can claim seats in both markets and earn
    /// yield from each — which is exactly what the multi-market test
    /// asserts. Same banks, same oracles; only the orderbook PDAs and
    /// per-market integration accounts differ.
    pub second_market: RefCell<Option<Keypair>>,
    /// Per-signer debt-mint ATA cache. `place_order_with_flags` synth-
    /// creates a debt-mint token account at a deterministic PDA for every
    /// signer on first call so the loader's owner/mint check on
    /// `borrower_debt_token` passes. Subsequent calls reuse the
    /// address so any P2Pool-borrowed atoms persist.
    signer_debt_tokens: RefCell<std::collections::HashMap<Pubkey, Pubkey>>,
}

#[allow(dead_code)] // test fixture; some helpers are reserved for future cases
impl MarketFixture {
    /// Boot ProgramTest with marginfi + ydelta loaded as SBPF binaries +
    /// the full mainnet fixture set, then run the `CreateMarket` ix to
    /// stand up a USDC-debt / wSOL-collateral market.
    pub async fn new() -> Self {
        ensure_marginfi_so_in_sbf_dir();

        let mut program = ProgramTest::default();
        program.prefer_bpf(true);
        program.add_program("marginfi", marginfi_mocks::ID, None);
        program.add_program("ydelta", ydelta::ID, None);

        for (pubkey, filename) in [
            (mainnet::marginfi_group(), "marginfi_group.json"),
            (mainnet::usdc_bank(), "marginfi_usdc_bank.json"),
            (mainnet::usdc_mint(), "usdc_mint.json"),
            (
                mainnet::usdc_liquidity_vault(),
                "marginfi_usdc_liquidity_vault.json",
            ),
            (mainnet::usdc_oracle(), "usdc_oracle.json"),
            (mainnet::sol_bank(), "marginfi_sol_bank.json"),
            (mainnet::wsol_mint(), "wsol_mint.json"),
            (
                mainnet::sol_liquidity_vault(),
                "marginfi_sol_liquidity_vault.json",
            ),
            (mainnet::sol_oracle(), "sol_oracle.json"),
        ] {
            let acct = load_account_from_fixture(filename);
            program.add_account(pubkey, acct);
        }

        let context = program.start_with_context().await;
        let payer = context.payer.insecure_clone();
        let market = Keypair::new();

        let fixture = Self {
            context: Rc::new(RefCell::new(context)),
            payer,
            market,
            second_market: RefCell::new(None),
            signer_debt_tokens: RefCell::new(std::collections::HashMap::new()),
        };
        fixture.create_global_config_account().await;
        fixture.create_market_account().await;
        fixture
    }

    /// Stand up a second `(USDC, wSOL)` market on this fixture. Used by
    /// the multi-market risk-profile test that asserts a profile in two
    /// markets earns the SUM of both lender interests. After calling,
    /// `second_market_pubkey()` returns the second market's address.
    pub async fn create_second_market(&self) {
        let market2 = Keypair::new();
        let payer = self.payer.insecure_clone();
        let ixs = create_market_instructions(
            &market2.pubkey(),
            &mainnet::usdc_mint(),
            &mainnet::wsol_mint(),
            &payer.pubkey(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &mainnet::sol_bank(),
            &marginfi_mocks::ID,
        )
        .unwrap();
        let m2_kp = market2.insecure_clone();
        self.process_ixs(&ixs, &[&m2_kp]).await.unwrap();
        // New markets ship paused; unpause for test ergonomics.
        let unpause = ydelta::program::instruction_builders::set_market_pause_instruction::set_market_pause_instruction(
            &market2.pubkey(), &payer.pubkey(), false,
        );
        self.process(unpause, &[]).await.unwrap();
        *self.second_market.borrow_mut() = Some(market2);
    }

    /// Pubkey of the second market. Panics if `create_second_market`
    /// hasn't been called yet.
    pub fn second_market_pubkey(&self) -> Pubkey {
        self.second_market
            .borrow()
            .as_ref()
            .expect("second market not created — call create_second_market() first")
            .pubkey()
    }

    /// Every state-mutating ix requires the singleton `GlobalConfig`
    /// PDA. Stand it up once at fixture setup; the `payer` becomes
    /// `protocol_admin`. Tests can flip `is_paused` via
    /// `set_global_pause_instruction`.
    async fn create_global_config_account(&self) {
        let payer = self.payer.insecure_clone();
        // Synthesize a fake ProgramData account so the
        // upgrade-authority gate passes under ProgramTest. See
        // tests/test_utils/fixture.rs for layout details.
        crate::test_utils::fixture::synth_program_data_account(&self.context, &payer.pubkey());
        let ix = ydelta::program::instruction_builders::global_config_admin_instructions::create_global_config_instruction(
            &payer.pubkey(),
        );
        self.process(ix, &[]).await.unwrap();
    }

    async fn create_market_account(&self) {
        let payer = self.payer.insecure_clone();
        let ixs = create_market_instructions(
            &self.market.pubkey(),
            &mainnet::usdc_mint(),
            &mainnet::wsol_mint(),
            &payer.pubkey(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &mainnet::sol_bank(),
            &marginfi_mocks::ID,
        )
        .unwrap();
        let market_kp = self.market.insecure_clone();
        self.process_ixs(&ixs, &[&market_kp]).await.unwrap();
        // New markets ship paused; unpause so the rest of the fixture
        // (loan lifecycle, order placement, etc.) can mutate market state.
        let unpause = ydelta::program::instruction_builders::set_market_pause_instruction::set_market_pause_instruction(
            &self.market.pubkey(), &payer.pubkey(), false,
        );
        self.process(unpause, &[]).await.unwrap();
    }

    /// Returns the lender-side marginfi-account PDA at
    /// `[b"marginfi_account", market.key]`. Phase-3 callers used
    /// `marginfi_account_pubkey` for the same address; the alias is kept
    /// for backwards compat during the Phase-4 migration.
    pub fn lender_marginfi_account_pubkey(&self) -> Pubkey {
        ydelta::validation::get_lender_integration_account_address(&self.market.pubkey()).0
    }

    /// Returns the borrower-side marginfi-account PDA at
    /// `[b"borrower_marginfi_account", market.key]`. Holds collateral as
    /// asset on the collateral bank + P2Pool liability on the debt bank.
    pub fn borrower_marginfi_account_pubkey(&self) -> Pubkey {
        ydelta::validation::get_borrower_integration_account_address(&self.market.pubkey()).0
    }

    /// **Deprecated** — Phase-3 alias for `lender_marginfi_account_pubkey`.
    pub fn marginfi_account_pubkey(&self) -> Pubkey {
        self.lender_marginfi_account_pubkey()
    }

    /// Return the synthetic debt-mint token account address for `signer`,
    /// creating it on first call. Used by Phase-4 `place_order` so the
    /// loader's owner/mint validation on `borrower_debt_token` passes.
    /// Atoms borrowed via `marginfi.borrow` (P2Pool fallback) accumulate
    /// here.
    pub fn signer_debt_token(&self, signer: &Pubkey) -> Pubkey {
        if let Some(addr) = self.signer_debt_tokens.borrow().get(signer) {
            return *addr;
        }
        let (addr, _) =
            Pubkey::find_program_address(&[b"test_debt_token", signer.as_ref()], &ydelta::ID);
        self.put_token_account(addr, mainnet::usdc_mint(), *signer, 0);
        self.signer_debt_tokens.borrow_mut().insert(*signer, addr);
        addr
    }

    pub fn market_signer_pubkey(&self) -> Pubkey {
        ydelta::validation::get_market_signer_address(&self.market.pubkey()).0
    }

    pub async fn process_ixs(
        &self,
        ixs: &[Instruction],
        extra_signers: &[&Keypair],
    ) -> Result<(), solana_program_test::BanksClientError> {
        let payer = self.payer.insecure_clone();
        let blockhash = self.context.borrow().last_blockhash;
        let mut signers: Vec<&Keypair> = vec![&payer];
        signers.extend_from_slice(extra_signers);
        let tx =
            Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &signers, blockhash);
        let client: RefMut<ProgramTestContext> = self.context.borrow_mut();
        client.banks_client.process_transaction(tx).await
    }

    pub async fn process(
        &self,
        ix: Instruction,
        extra_signers: &[&Keypair],
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.process_ixs(&[ix], extra_signers).await
    }

    /// Refresh both mainnet oracle fixtures so marginfi's staleness
    /// checks (and ydelta's match-time oracle reads) pass. Mirrors
    /// `AdapterFixture::refresh_oracle_freshness`.
    pub async fn refresh_oracle_freshness(&self) {
        let clock: solana_sdk::clock::Clock = {
            let ctx: RefMut<ProgramTestContext> = self.context.borrow_mut();
            ctx.banks_client.get_sysvar().await.unwrap()
        };
        // Pyth-pull `PriceUpdateV2`: publish_time at body+85, plus disc → 93.
        self.patch_oracle_pyth(mainnet::usdc_oracle(), clock.unix_timestamp)
            .await;
        // Switchboard-pull `PullFeedAccountData`: last_update_timestamp at
        // body+2208, plus disc → 2216. result.slot at body+2360 + disc → 2368.
        self.patch_oracle_swb(mainnet::sol_oracle(), clock.unix_timestamp, clock.slot)
            .await;
    }

    async fn patch_oracle_pyth(&self, address: Pubkey, timestamp: i64) {
        const PUBLISH_TIME_OFFSET: usize = 93;
        const PREV_PUBLISH_TIME_OFFSET: usize = 101;
        let mut ctx: RefMut<ProgramTestContext> = self.context.borrow_mut();
        let mut account = ctx
            .banks_client
            .get_account(address)
            .await
            .unwrap()
            .expect("pyth oracle fixture loaded");
        let bytes = timestamp.to_le_bytes();
        account.data[PUBLISH_TIME_OFFSET..PUBLISH_TIME_OFFSET + 8].copy_from_slice(&bytes);
        account.data[PREV_PUBLISH_TIME_OFFSET..PREV_PUBLISH_TIME_OFFSET + 8]
            .copy_from_slice(&bytes);
        ctx.set_account(&address, &account.into());
    }

    async fn patch_oracle_swb(&self, address: Pubkey, timestamp: i64, slot: u64) {
        const LAST_UPDATE_TIMESTAMP_OFFSET: usize = 2216;
        const RESULT_SLOT_OFFSET: usize = 2368;
        let mut ctx: RefMut<ProgramTestContext> = self.context.borrow_mut();
        let mut account = ctx
            .banks_client
            .get_account(address)
            .await
            .unwrap()
            .expect("switchboard oracle fixture loaded");
        account.data[LAST_UPDATE_TIMESTAMP_OFFSET..LAST_UPDATE_TIMESTAMP_OFFSET + 8]
            .copy_from_slice(&timestamp.to_le_bytes());
        account.data[RESULT_SLOT_OFFSET..RESULT_SLOT_OFFSET + 8]
            .copy_from_slice(&slot.to_le_bytes());
        ctx.set_account(&address, &account.into());
    }

    pub async fn refresh_blockhash(&self) {
        let mut client: RefMut<ProgramTestContext> = self.context.borrow_mut();
        let bh = client.banks_client.get_latest_blockhash().await.unwrap();
        client.last_blockhash = bh;
    }

    pub async fn account_data(&self, pubkey: Pubkey) -> Vec<u8> {
        let client: RefMut<ProgramTestContext> = self.context.borrow_mut();
        client
            .banks_client
            .get_account(pubkey)
            .await
            .unwrap()
            .expect("account exists")
            .data
    }

    pub async fn token_balance(&self, token_account: Pubkey) -> u64 {
        let data = self.account_data(token_account).await;
        u64::from_le_bytes(data[64..72].try_into().unwrap())
    }

    /// Drop a synthesised SPL token account at `pubkey` for `mint` owned
    /// by `owner` with `amount` atoms.
    pub fn put_token_account(&self, pubkey: Pubkey, mint: Pubkey, owner: Pubkey, amount: u64) {
        let mut data = vec![0u8; 165];
        data[0..32].copy_from_slice(mint.as_ref());
        data[32..64].copy_from_slice(owner.as_ref());
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        data[108] = 1; // state = Initialized
        let lamports = solana_sdk::rent::Rent::default().minimum_balance(165);
        let acc = Account {
            lamports,
            data,
            owner: spl_token::id(),
            executable: false,
            rent_epoch: 0,
        };
        self.context.borrow_mut().set_account(&pubkey, &acc.into());
    }

    /// Drop a synthesised **wSOL** token account at `pubkey` owned by
    /// `owner` with `amount` atoms. wSOL accounts carry the
    /// `is_native = Some(rent_exempt)` flag and store the wrapped SOL
    /// balance in lamports (= rent + amount), so SPL Transfer moves
    /// real lamports rather than just an `amount` field. Use this
    /// helper for wSOL ATAs; `put_token_account` zeroes `is_native`
    /// and produces inconsistent state for wSOL.
    pub fn put_wsol_token_account(&self, pubkey: Pubkey, owner: Pubkey, amount: u64) {
        let mut data = vec![0u8; 165];
        data[0..32].copy_from_slice(mainnet::wsol_mint().as_ref());
        data[32..64].copy_from_slice(owner.as_ref());
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        data[108] = 1; // state = Initialized
                       // is_native: Option<u64> at bytes 109..121 — Some(rent_exempt).
        let rent_exempt = solana_sdk::rent::Rent::default().minimum_balance(165);
        data[109..113].copy_from_slice(&1u32.to_le_bytes()); // Some discriminator
        data[113..121].copy_from_slice(&rent_exempt.to_le_bytes());
        // For native tokens lamports = rent_exempt + amount; SPL
        // Transfer moves lamports out alongside the amount field.
        let lamports = rent_exempt + amount;
        let acc = Account {
            lamports,
            data,
            owner: spl_token::id(),
            executable: false,
            rent_epoch: 0,
        };
        self.context.borrow_mut().set_account(&pubkey, &acc.into());
    }

    /// Create a fresh keypair pre-funded with SOL for rent. ATAs are
    /// synthesised separately via `put_token_account` since mainnet mints
    /// can't be minted to in tests.
    pub async fn create_trader(&self) -> Keypair {
        let trader = Keypair::new();
        let payer = self.payer.insecure_clone();
        let fund = solana_sdk::system_instruction::transfer(
            &payer.pubkey(),
            &trader.pubkey(),
            10_000_000_000,
        );
        self.process(fund, &[]).await.unwrap();
        trader
    }

    pub async fn claim_seat(&self, signer: &Keypair) {
        let ix = claim_seat_instruction(&self.market.pubkey(), &signer.pubkey());
        let kp = signer.insecure_clone();
        self.process(ix, &[&kp]).await.unwrap();
    }

    /// Variant of `claim_seat` for an arbitrary market (e.g. the
    /// secondary market created via `create_second_market`).
    pub async fn claim_seat_in_market(&self, signer: &Keypair, market_pk: Pubkey) {
        let ix = claim_seat_instruction(&market_pk, &signer.pubkey());
        let kp = signer.insecure_clone();
        self.process(ix, &[&kp]).await.unwrap();
    }

    /// Build + send a ydelta `Deposit` ix routing `amount_atoms` from
    /// `trader_token` (USDC or wSOL ATA) into the market's marginfi
    /// position.
    pub async fn deposit(
        &self,
        signer: &Keypair,
        trader_token: Pubkey,
        is_debt: bool,
        amount_atoms: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let (mint, bank, liquidity_vault) = if is_debt {
            (
                mainnet::usdc_mint(),
                mainnet::usdc_bank(),
                mainnet::usdc_liquidity_vault(),
            )
        } else {
            (
                mainnet::wsol_mint(),
                mainnet::sol_bank(),
                mainnet::sol_liquidity_vault(),
            )
        };
        let ix = deposit_instruction(
            &self.market.pubkey(),
            &signer.pubkey(),
            &mint,
            &mainnet::usdc_mint(),
            &trader_token,
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &bank,
            &liquidity_vault,
            &marginfi_mocks::ID,
            amount_atoms,
            None,
        );
        let kp = signer.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Variant of `deposit` for an arbitrary market.
    pub async fn deposit_in_market(
        &self,
        signer: &Keypair,
        market_pk: Pubkey,
        trader_token: Pubkey,
        is_debt: bool,
        amount_atoms: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let (mint, bank, liquidity_vault) = if is_debt {
            (
                mainnet::usdc_mint(),
                mainnet::usdc_bank(),
                mainnet::usdc_liquidity_vault(),
            )
        } else {
            (
                mainnet::wsol_mint(),
                mainnet::sol_bank(),
                mainnet::sol_liquidity_vault(),
            )
        };
        let ix = deposit_instruction(
            &market_pk,
            &signer.pubkey(),
            &mint,
            &mainnet::usdc_mint(),
            &trader_token,
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &bank,
            &liquidity_vault,
            &marginfi_mocks::ID,
            amount_atoms,
            None,
        );
        let kp = signer.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Build + send a ydelta `Withdraw` ix routing `amount_atoms` out of
    /// the market's marginfi position into `trader_token`.
    pub async fn withdraw(
        &self,
        signer: &Keypair,
        trader_token: Pubkey,
        is_debt: bool,
        amount_atoms: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let (mint, bank, liquidity_vault) = if is_debt {
            (
                mainnet::usdc_mint(),
                mainnet::usdc_bank(),
                mainnet::usdc_liquidity_vault(),
            )
        } else {
            (
                mainnet::wsol_mint(),
                mainnet::sol_bank(),
                mainnet::sol_liquidity_vault(),
            )
        };
        let bank_liquidity_vault_authority = mainnet::liquidity_vault_authority(bank);
        let ix = withdraw_instruction(
            &self.market.pubkey(),
            &signer.pubkey(),
            &mint,
            &mainnet::usdc_mint(),
            &trader_token,
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &mainnet::sol_bank(),
            &bank,
            &liquidity_vault,
            &bank_liquidity_vault_authority,
            &[mainnet::usdc_oracle()],
            &[mainnet::sol_oracle()],
            &marginfi_mocks::ID,
            amount_atoms,
            None,
        );
        let kp = signer.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn place_order(
        &self,
        signer: &Keypair,
        side: Side,
        order_type: OrderType,
        rate_bps: u16,
        term_seconds: u32,
        principal_atoms: u64,
        collateral_atoms: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        // Default to `OB_ONLY` so non-P2Pool tests don't accidentally
        // trigger the marginfi.borrow CPI. Tests that exercise the
        // P2Pool fallback call `place_order_with_flags(..., 0)`
        // explicitly.
        self.place_order_with_flags(
            signer,
            side,
            order_type,
            rate_bps,
            term_seconds,
            principal_atoms,
            collateral_atoms,
            ydelta::state::market_helpers::FLAG_OB_ONLY,
        )
        .await
    }

    /// Place-order variant exposing the borrower-LTV cap. `Some(bps)`
    /// declares an explicit cap; `None` falls through to the
    /// processor's default (marginfi-init).
    #[allow(clippy::too_many_arguments)]
    pub async fn place_order_with_borrower_ltv(
        &self,
        signer: &Keypair,
        side: Side,
        order_type: OrderType,
        rate_bps: u16,
        term_seconds: u32,
        principal_atoms: u64,
        collateral_atoms: u64,
        borrower_ltv_bps: Option<u16>,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.refresh_oracle_freshness().await;
        let debt_bank_lva = mainnet::liquidity_vault_authority(mainnet::usdc_bank());
        let borrower_debt_token = self.signer_debt_token(&signer.pubkey());
        let ix = place_order_instruction(
            &self.market.pubkey(),
            &signer.pubkey(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &mainnet::sol_bank(),
            &[mainnet::usdc_oracle()],
            &[mainnet::sol_oracle()],
            &mainnet::usdc_liquidity_vault(),
            &debt_bank_lva,
            &borrower_debt_token,
            &mainnet::usdc_mint(),
            &spl_token::id(),
            &marginfi_mocks::ID,
            side,
            order_type,
            rate_bps,
            term_seconds,
            principal_atoms,
            collateral_atoms,
            0,
            ydelta::state::market_helpers::FLAG_OB_ONLY,
            None,
            borrower_ltv_bps,
        );
        let kp = signer.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Place-order variant that exposes the `flags` byte. Used by
    /// Step-10 tests for the `OB_ONLY` flag (`flags & 0x02`).
    #[allow(clippy::too_many_arguments)]
    pub async fn place_order_with_flags(
        &self,
        signer: &Keypair,
        side: Side,
        order_type: OrderType,
        rate_bps: u16,
        term_seconds: u32,
        principal_atoms: u64,
        collateral_atoms: u64,
        flags: u8,
    ) -> Result<(), solana_program_test::BanksClientError> {
        // place_order reads both oracles for the LTV check at match
        // time. The dumped fixture timestamps are stale relative to
        // the test ledger's Clock — refresh them before every
        // place_order call so the staleness gate passes.
        self.refresh_oracle_freshness().await;
        let debt_bank_lva = mainnet::liquidity_vault_authority(mainnet::usdc_bank());
        let borrower_debt_token = self.signer_debt_token(&signer.pubkey());
        let ix = place_order_instruction(
            &self.market.pubkey(),
            &signer.pubkey(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &mainnet::sol_bank(),
            &[mainnet::usdc_oracle()],
            &[mainnet::sol_oracle()],
            &mainnet::usdc_liquidity_vault(),
            &debt_bank_lva,
            &borrower_debt_token,
            &mainnet::usdc_mint(),
            &spl_token::id(),
            &marginfi_mocks::ID,
            side,
            order_type,
            rate_bps,
            term_seconds,
            principal_atoms,
            collateral_atoms,
            0,
            flags,
            None,
            None, // borrower_ltv_bps default — marginfi-init
        );
        let kp = signer.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Variant of `place_order_with_flags` for an arbitrary market.
    #[allow(clippy::too_many_arguments)]
    pub async fn place_order_with_flags_in_market(
        &self,
        signer: &Keypair,
        market_pk: Pubkey,
        side: Side,
        order_type: OrderType,
        rate_bps: u16,
        term_seconds: u32,
        principal_atoms: u64,
        collateral_atoms: u64,
        flags: u8,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.refresh_oracle_freshness().await;
        let debt_bank_lva = mainnet::liquidity_vault_authority(mainnet::usdc_bank());
        let borrower_debt_token = self.signer_debt_token(&signer.pubkey());
        let ix = place_order_instruction(
            &market_pk,
            &signer.pubkey(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &mainnet::sol_bank(),
            &[mainnet::usdc_oracle()],
            &[mainnet::sol_oracle()],
            &mainnet::usdc_liquidity_vault(),
            &debt_bank_lva,
            &borrower_debt_token,
            &mainnet::usdc_mint(),
            &spl_token::id(),
            &marginfi_mocks::ID,
            side,
            order_type,
            rate_bps,
            term_seconds,
            principal_atoms,
            collateral_atoms,
            0,
            flags,
            None,
            None,
        );
        let kp = signer.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Variant of `repay` for an arbitrary market.
    pub async fn repay_in_market(
        &self,
        borrower: &Keypair,
        market_pk: Pubkey,
        sequence: u64,
        borrower_token: Pubkey,
        repay_atoms: u64,
        full_repay: bool,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = repay_instruction(
            &market_pk,
            &borrower.pubkey(),
            sequence,
            &mainnet::usdc_mint(),
            &borrower_token,
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &mainnet::usdc_liquidity_vault(),
            &mainnet::sol_bank(),
            &marginfi_mocks::ID,
            repay_atoms,
            full_repay,
            None,
        );
        let kp = borrower.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Read a loan PDA from an arbitrary market.
    pub async fn read_loan_in(
        &self,
        market_pk: Pubkey,
        sequence: u64,
    ) -> ydelta::state::loan::LoanFixed {
        let (loan_addr, _) = ydelta::state::loan::loan_pda(&market_pk, sequence);
        let data = self.account_data(loan_addr).await;
        let header: &ydelta::state::loan::LoanFixed =
            bytemuck::from_bytes(&data[..ydelta::state::loan::LOAN_FIXED_SIZE]);
        *header
    }

    /// Place a `SecondaryLoanSale` bid. `seller` is the loan's
    /// current lender; `loan_pda` derived via
    /// `ydelta::state::loan::loan_pda(market, sequence)`. Engine
    /// reads the loan PDA and snapshots rate/term/principal. Pricing
    /// is fixed at par; the seller's only exit cost is the
    /// accrued-interest seizure under Option A.
    pub async fn place_secondary_order(
        &self,
        seller: &Keypair,
        loan_sequence: u64,
        last_valid_unix_ts: i64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.refresh_oracle_freshness().await;
        let (loan_pda, _) = ydelta::state::loan::loan_pda(&self.market.pubkey(), loan_sequence);
        let debt_bank_lva = mainnet::liquidity_vault_authority(mainnet::usdc_bank());
        let borrower_debt_token = self.signer_debt_token(&seller.pubkey());
        let ix = secondary_place_order_instruction(
            &self.market.pubkey(),
            &seller.pubkey(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &mainnet::sol_bank(),
            &[mainnet::usdc_oracle()],
            &[mainnet::sol_oracle()],
            &mainnet::usdc_liquidity_vault(),
            &debt_bank_lva,
            &borrower_debt_token,
            &mainnet::usdc_mint(),
            &spl_token::id(),
            &marginfi_mocks::ID,
            &loan_pda,
            last_valid_unix_ts,
            /*flags=*/ 0,
            None,
        );
        let kp = seller.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Update a `SecondaryLoanSale` bid's mutable fields.
    /// Only `last_valid_unix_ts` (expiry touch) is mutable; rate /
    /// term / principal / collateral / asking_price are all fixed at
    /// placement and any mutation attempt is rejected by the processor
    /// with `SecondaryFieldImmutable`.
    pub async fn update_secondary_order(
        &self,
        seller: &Keypair,
        order_sequence_number: u64,
        new_last_valid_unix_ts: Option<i64>,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = update_secondary_order_instruction(
            &self.market.pubkey(),
            &seller.pubkey(),
            order_sequence_number,
            None,
            None,
            new_last_valid_unix_ts,
            None,
        );
        let kp = seller.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Test-only — try to update a SecondaryLoanSale bid with
    /// `new_rate_bps` set. Used to verify the SecondaryFieldImmutable
    /// guard rejects the call. Bypasses the secondary-only builder and
    /// goes through the primary `update_order_instruction`.
    pub async fn try_update_secondary_with_rate(
        &self,
        seller: &Keypair,
        order_sequence_number: u64,
        new_rate_bps: u16,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = ydelta::program::instruction_builders::update_order_instruction::update_order_instruction(
            &self.market.pubkey(),
            &seller.pubkey(),
            order_sequence_number,
            None,
            None,
            Some(new_rate_bps),
            None,
            None,
            None,
            None,
            None,
        );
        let kp = seller.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Cancel a resting order by its sequence number. Mirrors the
    /// primary cancel path; works for SecondaryLoanSale bids too
    /// (`cancel_order_by_index` skips the unencumber step for them).
    pub async fn cancel_order(
        &self,
        signer: &Keypair,
        order_sequence_number: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.cancel_order_with_optional_loan(signer, order_sequence_number, None)
            .await
    }

    /// Explicit form for canceling a `SecondaryLoanSale` bid. The
    /// loan PDA is required so the processor can clear
    /// `has_resting_secondary_bid`.
    pub async fn cancel_secondary_order(
        &self,
        signer: &Keypair,
        order_sequence_number: u64,
        loan_pda: solana_program::pubkey::Pubkey,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.cancel_order_with_optional_loan(signer, order_sequence_number, Some(loan_pda))
            .await
    }

    async fn cancel_order_with_optional_loan(
        &self,
        signer: &Keypair,
        order_sequence_number: u64,
        loan_pda: Option<solana_program::pubkey::Pubkey>,
    ) -> Result<(), solana_program_test::BanksClientError> {
        use ydelta::program::instruction_builders::cancel_order_instruction::cancel_order_instruction;
        let ix = cancel_order_instruction(
            &self.market.pubkey(),
            &signer.pubkey(),
            order_sequence_number,
            None,
            None,
            loan_pda,
        );
        let kp = signer.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Walk the bids tree to find any resting `SecondaryLoanSale` for
    /// the given loan sequence; return its `sequence_number` field.
    /// `None` if no match.
    pub async fn read_first_bid_sequence_for_loan(&self, loan_sequence: u64) -> Option<u64> {
        use hypertree::HyperTreeValueIteratorTrait;
        use ydelta::state::market::BooksideReadOnly;
        use ydelta::state::resting_order::{OrderKind, RestingOrder};

        let (loan_pda, _) = ydelta::state::loan::loan_pda(&self.market.pubkey(), loan_sequence);
        let data = self.account_data(self.market.pubkey()).await;
        let fixed_size = std::mem::size_of::<ydelta::state::MarketFixed>();
        let header: &ydelta::state::MarketFixed = bytemuck::from_bytes(&data[..fixed_size]);
        let bids_root = header.bids_root_index;
        let bids_best = header.bids_best_index;
        let dynamic = &data[fixed_size..];
        let tree = BooksideReadOnly::new(dynamic, bids_root, bids_best);
        for (_idx, order) in tree.iter::<RestingOrder>() {
            if order.kind == OrderKind::SecondaryLoanSale && order.loan_pda == loan_pda {
                return Some(order.sequence_number);
            }
        }
        None
    }

    /// Read the on-disk `ClaimedSeat` body at the given index.
    pub async fn read_seat_data(&self, index: hypertree::DataIndex) -> ydelta::state::ClaimedSeat {
        let data = self.account_data(self.market.pubkey()).await;
        let fixed_size = std::mem::size_of::<ydelta::state::MarketFixed>();
        let dynamic = &data[fixed_size..];
        *ydelta::state::market::get_helper_seat(dynamic, index).get_value()
    }

    /// Look up a trader's seat index. Returns `NIL` if not claimed.
    pub async fn read_seat_index(&self, owner: &Pubkey) -> hypertree::DataIndex {
        use hypertree::{HyperTreeReadOperations, NIL};
        let data = self.account_data(self.market.pubkey()).await;
        let fixed_size = std::mem::size_of::<ydelta::state::MarketFixed>();
        let header: &ydelta::state::MarketFixed = bytemuck::from_bytes(&data[..fixed_size]);
        let root = header.claimed_seats_root_index;
        let dynamic = &data[fixed_size..];
        let tree = ydelta::state::market::ClaimedSeatTreeReadOnly::new(dynamic, root, NIL);
        let probe = ydelta::state::ClaimedSeat::new_empty(*owner, 0, 0);
        tree.lookup_index(&probe)
    }

    /// Crank a PRIMARY `MatchedLoan` queue entry into a `LoanFixed`
    /// PDA. Anyone can sign as `cranker` (payer); ydelta-test default
    /// is the fixture's payer keypair.
    pub async fn crank_matched_loan(
        &self,
        sequence: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = process_matched_loan_instruction(
            &self.market.pubkey(),
            &self.payer.pubkey(),
            &mainnet::usdc_bank(),
            &marginfi_mocks::ID,
            sequence,
            None,
            None,
        );
        self.process(ix, &[]).await
    }

    /// Cranker promote for a risk-profile-funded MatchedLoan. Threads
    /// the trailing vault-settle accounts so `do_vault_settle` can run
    /// the 3-CPI atom migration (vault.integration → global_vault_staging →
    /// market_debt_vault → market.lender_integration_account).
    pub async fn crank_matched_loan_for_risk_profile(
        &self,
        sequence: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        use ydelta::program::instruction_builders::process_matched_loan_instruction::VaultSettleAddrs;
        use ydelta::state::vault::{
            global_vault_integration_account_pda, global_vault_pda, global_vault_signer_pda,
            global_vault_staging_pda,
        };
        use ydelta::validation::{
            get_lender_integration_account_address, get_market_signer_address, get_vault_address,
        };
        self.refresh_oracle_freshness().await;
        let market_pk = self.market.pubkey();
        let (gv, _) = global_vault_pda(&mainnet::usdc_mint());
        let (gv_signer, _) = global_vault_signer_pda(&gv);
        let (gv_staging, _) = global_vault_staging_pda(&gv);
        let (gv_integration, _) = global_vault_integration_account_pda(&gv);
        let (market_debt_vault, _) = get_vault_address(&market_pk, &mainnet::usdc_mint());
        let (market_signer, _) = get_market_signer_address(&market_pk);
        let (market_lender_mfi, _) = get_lender_integration_account_address(&market_pk);
        let (debt_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::usdc_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let ix = process_matched_loan_instruction(
            &market_pk,
            &self.payer.pubkey(),
            &mainnet::usdc_bank(),
            &marginfi_mocks::ID,
            sequence,
            None,
            Some(VaultSettleAddrs {
                global_vault: gv,
                global_vault_signer: gv_signer,
                global_vault_staging: gv_staging,
                global_vault_integration_account: gv_integration,
                market_debt_vault,
                market_lender_integration_account: market_lender_mfi,
                market_signer,
                debt_liquidity_vault: mainnet::usdc_liquidity_vault(),
                debt_bank_liquidity_vault_authority: debt_bank_lva,
                debt_oracles: vec![mainnet::usdc_oracle()],
                debt_mint: mainnet::usdc_mint(),
                token_program: spl_token::id(),
                marginfi_group: mainnet::marginfi_group(),
                marginfi_program: marginfi_mocks::ID,
            }),
        );
        self.process(ix, &[]).await
    }

    /// Permissionless cranker — drains a fully-repaid risk-profile loan
    /// from `market.lender_integration_account` back into the
    /// GlobalVault's marginfi account, decrements per-market exposure
    /// usage, sweeps protocol-fee shares onto the market accumulator,
    /// closes the loan PDA. Caller is the signer; pass any keypair.
    pub async fn claim_repayment_for_risk_profile(
        &self,
        cranker: &Keypair,
        sequence: u64,
        cranker_refund: Option<Pubkey>,
    ) -> Result<(), solana_program_test::BanksClientError> {
        use solana_sdk::compute_budget::ComputeBudgetInstruction;
        use ydelta::program::instruction_builders::claim_repayment_for_risk_profile_instruction::claim_repayment_for_risk_profile_instruction;
        use ydelta::state::vault::global_vault_pda;
        use ydelta::validation::get_lender_integration_account_address;
        self.refresh_oracle_freshness().await;
        let market_pk = self.market.pubkey();
        let (gv, _) = global_vault_pda(&mainnet::usdc_mint());
        let (lender_mfi, _) = get_lender_integration_account_address(&market_pk);
        let (debt_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::usdc_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let ix = claim_repayment_for_risk_profile_instruction(
            &cranker.pubkey(),
            &market_pk,
            sequence,
            &gv,
            &mainnet::usdc_mint(),
            &mainnet::usdc_bank(),
            &mainnet::usdc_liquidity_vault(),
            &debt_bank_lva,
            &mainnet::usdc_oracle(),
            &lender_mfi,
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &marginfi_mocks::ID,
            cranker_refund.as_ref(),
        );
        // Three marginfi CPIs (withdraw, deposit) + SPL transfer push
        // the ix above the 200k default; bump to 400k for headroom.
        let cu = ComputeBudgetInstruction::set_compute_unit_limit(400_000);
        let kp = cranker.insecure_clone();
        self.process_ixs(&[cu, ix], &[&kp]).await
    }

    /// Variant of `crank_matched_loan_for_risk_profile` for an
    /// arbitrary market. Used by the multi-market test.
    pub async fn crank_matched_loan_for_risk_profile_in_market(
        &self,
        market_pk: Pubkey,
        sequence: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        use ydelta::program::instruction_builders::process_matched_loan_instruction::VaultSettleAddrs;
        use ydelta::state::vault::{
            global_vault_integration_account_pda, global_vault_pda, global_vault_signer_pda,
            global_vault_staging_pda,
        };
        use ydelta::validation::{
            get_lender_integration_account_address, get_market_signer_address, get_vault_address,
        };
        self.refresh_oracle_freshness().await;
        let (gv, _) = global_vault_pda(&mainnet::usdc_mint());
        let (gv_signer, _) = global_vault_signer_pda(&gv);
        let (gv_staging, _) = global_vault_staging_pda(&gv);
        let (gv_integration, _) = global_vault_integration_account_pda(&gv);
        let (market_debt_vault, _) = get_vault_address(&market_pk, &mainnet::usdc_mint());
        let (market_signer, _) = get_market_signer_address(&market_pk);
        let (market_lender_mfi, _) = get_lender_integration_account_address(&market_pk);
        let (debt_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::usdc_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let ix = process_matched_loan_instruction(
            &market_pk,
            &self.payer.pubkey(),
            &mainnet::usdc_bank(),
            &marginfi_mocks::ID,
            sequence,
            None,
            Some(VaultSettleAddrs {
                global_vault: gv,
                global_vault_signer: gv_signer,
                global_vault_staging: gv_staging,
                global_vault_integration_account: gv_integration,
                market_debt_vault,
                market_lender_integration_account: market_lender_mfi,
                market_signer,
                debt_liquidity_vault: mainnet::usdc_liquidity_vault(),
                debt_bank_liquidity_vault_authority: debt_bank_lva,
                debt_oracles: vec![mainnet::usdc_oracle()],
                debt_mint: mainnet::usdc_mint(),
                token_program: spl_token::id(),
                marginfi_group: mainnet::marginfi_group(),
                marginfi_program: marginfi_mocks::ID,
            }),
        );
        self.process(ix, &[]).await
    }

    /// Variant of `claim_repayment_for_risk_profile` for an arbitrary
    /// market.
    pub async fn claim_repayment_for_risk_profile_in_market(
        &self,
        cranker: &Keypair,
        market_pk: Pubkey,
        sequence: u64,
        cranker_refund: Option<Pubkey>,
    ) -> Result<(), solana_program_test::BanksClientError> {
        use solana_sdk::compute_budget::ComputeBudgetInstruction;
        use ydelta::program::instruction_builders::claim_repayment_for_risk_profile_instruction::claim_repayment_for_risk_profile_instruction;
        use ydelta::state::vault::global_vault_pda;
        use ydelta::validation::get_lender_integration_account_address;
        self.refresh_oracle_freshness().await;
        let (gv, _) = global_vault_pda(&mainnet::usdc_mint());
        let (lender_mfi, _) = get_lender_integration_account_address(&market_pk);
        let (debt_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::usdc_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let ix = claim_repayment_for_risk_profile_instruction(
            &cranker.pubkey(),
            &market_pk,
            sequence,
            &gv,
            &mainnet::usdc_mint(),
            &mainnet::usdc_bank(),
            &mainnet::usdc_liquidity_vault(),
            &debt_bank_lva,
            &mainnet::usdc_oracle(),
            &lender_mfi,
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &marginfi_mocks::ID,
            cranker_refund.as_ref(),
        );
        let cu = ComputeBudgetInstruction::set_compute_unit_limit(400_000);
        let kp = cranker.insecure_clone();
        self.process_ixs(&[cu, ix], &[&kp]).await
    }

    /// Crank a SECONDARY (full transfer) queue node where the new
    /// lender is a wallet seat. `referenced_loan_sequence` is the
    /// existing loan being transferred.
    pub async fn crank_secondary_matched_loan(
        &self,
        queue_sequence: u64,
        referenced_loan_sequence: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.refresh_oracle_freshness().await;
        let ix = process_secondary_matched_loan_instruction(
            &self.market.pubkey(),
            &self.payer.pubkey(),
            queue_sequence,
            referenced_loan_sequence,
            &mainnet::usdc_bank(),
            &mainnet::sol_bank(),
            &[mainnet::usdc_oracle()],
            &[mainnet::sol_oracle()],
            &marginfi_mocks::ID,
            None,
            None,
        );
        self.process(ix, &[]).await
    }

    /// Crank a SECONDARY (full transfer) queue node where the new
    /// lender is a risk-profile seat. Threads the trailing vault-settle
    /// accounts so the cranker can migrate the buyer's cash atoms from
    /// `vault.integration` into `market.lender_integration_account` for
    /// the seller's payout.
    pub async fn crank_secondary_matched_loan_for_risk_profile_buyer(
        &self,
        queue_sequence: u64,
        referenced_loan_sequence: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        use solana_sdk::compute_budget::ComputeBudgetInstruction;
        use ydelta::program::instruction_builders::process_matched_loan_instruction::VaultSettleAddrs;
        use ydelta::state::vault::{
            global_vault_integration_account_pda, global_vault_pda, global_vault_signer_pda,
            global_vault_staging_pda,
        };
        use ydelta::validation::{
            get_lender_integration_account_address, get_market_signer_address, get_vault_address,
        };
        self.refresh_oracle_freshness().await;
        let market_pk = self.market.pubkey();
        let (gv, _) = global_vault_pda(&mainnet::usdc_mint());
        let (gv_signer, _) = global_vault_signer_pda(&gv);
        let (gv_staging, _) = global_vault_staging_pda(&gv);
        let (gv_integration, _) = global_vault_integration_account_pda(&gv);
        let (market_debt_vault, _) = get_vault_address(&market_pk, &mainnet::usdc_mint());
        let (market_signer, _) = get_market_signer_address(&market_pk);
        let (market_lender_mfi, _) = get_lender_integration_account_address(&market_pk);
        let (debt_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::usdc_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let ix = process_secondary_matched_loan_instruction(
            &market_pk,
            &self.payer.pubkey(),
            queue_sequence,
            referenced_loan_sequence,
            &mainnet::usdc_bank(),
            &mainnet::sol_bank(),
            &[mainnet::usdc_oracle()],
            &[mainnet::sol_oracle()],
            &marginfi_mocks::ID,
            None,
            Some(VaultSettleAddrs {
                global_vault: gv,
                global_vault_signer: gv_signer,
                global_vault_staging: gv_staging,
                global_vault_integration_account: gv_integration,
                market_debt_vault,
                market_lender_integration_account: market_lender_mfi,
                market_signer,
                debt_liquidity_vault: mainnet::usdc_liquidity_vault(),
                debt_bank_liquidity_vault_authority: debt_bank_lva,
                debt_oracles: vec![mainnet::usdc_oracle()],
                debt_mint: mainnet::usdc_mint(),
                token_program: spl_token::id(),
                marginfi_group: mainnet::marginfi_group(),
                marginfi_program: marginfi_mocks::ID,
            }),
        );
        let cu = ComputeBudgetInstruction::set_compute_unit_limit(400_000);
        self.process_ixs(&[cu, ix], &[]).await
    }

    /// Crank a SECONDARY|SPLIT queue node.
    /// `next_market_sequence` is the live `market.matched_loan_sequence`
    /// at submission time; the cranker validates the passed split PDA
    /// matches that derivation.
    #[allow(clippy::too_many_arguments)]
    pub async fn crank_secondary_split_matched_loan(
        &self,
        queue_sequence: u64,
        referenced_loan_sequence: u64,
        next_market_sequence: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.refresh_oracle_freshness().await;
        let ix = process_secondary_split_matched_loan_instruction(
            &self.market.pubkey(),
            &self.payer.pubkey(),
            queue_sequence,
            referenced_loan_sequence,
            next_market_sequence,
            &mainnet::usdc_bank(),
            &mainnet::sol_bank(),
            &[mainnet::usdc_oracle()],
            &[mainnet::sol_oracle()],
            &marginfi_mocks::ID,
            None,
        );
        self.process(ix, &[]).await
    }

    /// Fast-forward the test ledger's `Clock` so loans can move past
    /// their `matures_at_unix` boundary. Sets the sysvar directly via
    /// `set_sysvar`. Subsequent ixs see `now = new_unix_ts` until
    /// `set_sysvar` is called again or the test ends.
    pub async fn set_clock_unix_timestamp(&self, new_unix_ts: i64) {
        let ctx: RefMut<ProgramTestContext> = self.context.borrow_mut();
        let mut clock: solana_sdk::clock::Clock = ctx.banks_client.get_sysvar().await.unwrap();
        clock.unix_timestamp = new_unix_ts;
        ctx.set_sysvar(&clock);
    }

    /// Borrower repays a fixed loan. `full_repay = true` overrides
    /// `repay_atoms` with the loan's outstanding balance at process
    /// time.
    pub async fn repay(
        &self,
        borrower: &Keypair,
        sequence: u64,
        borrower_token: Pubkey,
        repay_atoms: u64,
        full_repay: bool,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = repay_instruction(
            &self.market.pubkey(),
            &borrower.pubkey(),
            sequence,
            &mainnet::usdc_mint(),
            &borrower_token,
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &mainnet::usdc_liquidity_vault(),
            &mainnet::sol_bank(),
            &marginfi_mocks::ID,
            repay_atoms,
            full_repay,
            None,
        );
        let kp = borrower.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Lender claims their accrued atoms from a repaid loan.
    pub async fn claim_repayment(
        &self,
        lender: &Keypair,
        sequence: u64,
        cranker_refund: Option<Pubkey>,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = claim_repayment_instruction(
            &self.market.pubkey(),
            &lender.pubkey(),
            sequence,
            &mainnet::usdc_bank(),
            &marginfi_mocks::ID,
            cranker_refund.as_ref(),
            None,
        );
        let kp = lender.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Invoke `liquidate_loan` on a live loan with the passed
    /// `repay_atoms_max` (0 = full repay). Returns the transaction
    /// result so callers can assert success or specific error codes.
    pub async fn liquidate_loan(
        &self,
        liquidator: &Keypair,
        sequence: u64,
        liquidator_debt_token: Pubkey,
        liquidator_collateral_token: Pubkey,
        repay_atoms_max: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let (debt_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::usdc_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let (coll_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::sol_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let _ = debt_bank_lva;
        let ix = liquidate_loan_instruction(
            &self.market.pubkey(),
            &liquidator.pubkey(),
            sequence,
            &mainnet::usdc_mint(),
            &mainnet::wsol_mint(),
            &liquidator_debt_token,
            &liquidator_collateral_token,
            &mainnet::usdc_bank(),
            &mainnet::sol_bank(),
            &mainnet::usdc_liquidity_vault(),
            &mainnet::sol_liquidity_vault(),
            &coll_bank_lva,
            &[mainnet::usdc_oracle()],
            &[mainnet::sol_oracle()],
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &marginfi_mocks::ID,
            repay_atoms_max,
        );
        let kp = liquidator.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Run `SettleMaturedLoan` (tag 20). Permissionless cranker action
    /// that retires a Fixed loan past `matures_at + grace_period_seconds`:
    /// withdraws collateral atoms, swaps via marginfi, applies a
    /// liquidator bonus, and credits the lender. Pass `repay_atoms_max=0`
    /// to clear the entire outstanding debt; nonzero caps the repay
    /// (residual stays Active for the next call).
    pub async fn settle_matured_loan(
        &self,
        liquidator: &Keypair,
        sequence: u64,
        liquidator_debt_token: Pubkey,
        liquidator_collateral_token: Pubkey,
        repay_atoms_max: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        use solana_sdk::compute_budget::ComputeBudgetInstruction;
        use ydelta::program::instruction_builders::settle_matured_loan_instruction::settle_matured_loan_instruction;
        let (_debt_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::usdc_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let (coll_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::sol_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let ix = settle_matured_loan_instruction(
            &self.market.pubkey(),
            &liquidator.pubkey(),
            sequence,
            &mainnet::usdc_mint(),
            &mainnet::wsol_mint(),
            &liquidator_debt_token,
            &liquidator_collateral_token,
            &mainnet::usdc_bank(),
            &mainnet::sol_bank(),
            &mainnet::usdc_liquidity_vault(),
            &mainnet::sol_liquidity_vault(),
            &coll_bank_lva,
            &[mainnet::usdc_oracle()],
            &[mainnet::sol_oracle()],
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &marginfi_mocks::ID,
            repay_atoms_max,
        );
        // Settle does marginfi.withdraw (collateral) + deposit (debt
        // for repay) + repay + transfers + seat updates — pushes well
        // above the 200k default ix budget.
        let cu = ComputeBudgetInstruction::set_compute_unit_limit(400_000);
        let kp = liquidator.insecure_clone();
        self.process_ixs(&[cu, ix], &[&kp]).await
    }

    /// Run `ConvertP2PoolToFixed` (tag 39). `loan_sequence` identifies
    /// the P2Pool loan PDA. The ix walks the asks tree and crosses every
    /// compatible wallet ask whose `rate_bps <= max_acceptable_rate_bps`
    /// AND `term_seconds >= (loan.matures_at - now)`; each cross emits
    /// a Fixed MatchedLoan (cranker promotes). Pass
    /// `Some(loan.created_by)` to recover P2Pool PDA rent on full
    /// conversion; `None` skips the refund.
    pub async fn convert_p2pool_to_fixed(
        &self,
        borrower: &Keypair,
        loan_sequence: u64,
        max_acceptable_rate_bps: u16,
        cranker_refund: Option<Pubkey>,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let (debt_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::usdc_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let ix = ydelta::program::instruction_builders::convert_p2pool_to_fixed_instruction::convert_p2pool_to_fixed_instruction(
            &self.market.pubkey(),
            &borrower.pubkey(),
            loan_sequence,
            &mainnet::usdc_mint(),
            &mainnet::usdc_bank(),
            &mainnet::usdc_liquidity_vault(),
            &debt_bank_lva,
            &[mainnet::usdc_oracle()],
            &mainnet::sol_bank(),
            &[mainnet::sol_oracle()],
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &marginfi_mocks::ID,
            max_acceptable_rate_bps,
            cranker_refund.as_ref(),
        );
        let kp = borrower.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Overwrite the Switchboard CurrentResult.value (the price in
    /// fp18 decimal scale) on a Switchboard-pull oracle. Used by
    /// liquidation tests to crash the collateral oracle below the
    /// maint-LTV threshold without rebuilding the fixture.
    pub async fn set_swb_oracle_price_atoms(&self, address: Pubkey, value_fp18: i128) {
        // Layout: 8-byte discriminator + body. Inside body:
        // CurrentResult.value at offset 2256 → account-data offset 2264.
        const VALUE_OFFSET: usize = 8 + 2256;
        let mut ctx: RefMut<ProgramTestContext> = self.context.borrow_mut();
        let mut account = ctx
            .banks_client
            .get_account(address)
            .await
            .unwrap()
            .expect("switchboard oracle fixture loaded");
        account.data[VALUE_OFFSET..VALUE_OFFSET + 16].copy_from_slice(&value_fp18.to_le_bytes());
        ctx.set_account(&address, &account.into());
    }

    /// Look up a loan account's on-chain bytes — useful to assert it's
    /// gone (zeroed discriminator) after a closing claim.
    pub async fn loan_account_data(&self, sequence: u64) -> Vec<u8> {
        let (loan_addr, _) = ydelta::state::loan::loan_pda(&self.market.pubkey(), sequence);
        self.account_data(loan_addr).await
    }

    /// Read the market account's `MarketFixed` header.
    pub async fn read_market_fixed(&self) -> ydelta::state::MarketFixed {
        let data = self.account_data(self.market.pubkey()).await;
        let fixed_size = std::mem::size_of::<ydelta::state::MarketFixed>();
        *bytemuck::from_bytes::<ydelta::state::MarketFixed>(&data[..fixed_size])
    }

    /// Read the on-disk LoanFixed at the per-(market, sequence) PDA.
    pub async fn read_loan(&self, sequence: u64) -> ydelta::state::loan::LoanFixed {
        let (loan_addr, _) = ydelta::state::loan::loan_pda(&self.market.pubkey(), sequence);
        let data = self.account_data(loan_addr).await;
        *bytemuck::from_bytes::<ydelta::state::loan::LoanFixed>(
            &data[..std::mem::size_of::<ydelta::state::loan::LoanFixed>()],
        )
    }

    /// Read a split sub-loan by its STABLE queue-node sequence. Splits
    /// derive their PDA from `[SPLIT_LOAN_SEED, market, queue_seq]`
    /// instead of `[LOAN_SEED, market, matched_loan_sequence]` (per
    /// race-resistance against concurrent place_orders).
    pub async fn read_split_loan(&self, queue_sequence: u64) -> ydelta::state::loan::LoanFixed {
        let (split_addr, _) =
            ydelta::state::loan::split_loan_pda(&self.market.pubkey(), queue_sequence);
        let data = self.account_data(split_addr).await;
        *bytemuck::from_bytes::<ydelta::state::loan::LoanFixed>(
            &data[..std::mem::size_of::<ydelta::state::loan::LoanFixed>()],
        )
    }

    /// Test backdoor: bump a seat's `*_withdrawable_shares` directly,
    /// bypassing the deposit ix. Used by tests that need pre-existing
    /// seat balance to drive matching/encumbrance flows but don't want
    /// to push the actual atoms through marginfi (e.g. for sides where
    /// the bank's interest math overflows on synthetic test deposits).
    pub async fn seed_seat_shares(&self, owner: &Pubkey, shares: u128, is_debt: bool) {
        use hypertree::{HyperTreeReadOperations, RedBlackTreeReadOnly, NIL};
        use ydelta::state::market::get_mut_helper_seat;
        use ydelta::state::{ClaimedSeat, MarketFixed};

        let mut data = {
            let client: RefMut<ProgramTestContext> = self.context.borrow_mut();
            client
                .banks_client
                .get_account(self.market.pubkey())
                .await
                .unwrap()
                .unwrap()
                .data
        };

        let fixed_size = std::mem::size_of::<MarketFixed>();
        let claimed_seats_root_index = {
            let header: &MarketFixed = bytemuck::from_bytes(&data[..fixed_size]);
            header.claimed_seats_root_index
        };

        let (_fixed_bytes, dyn_bytes) = data.split_at_mut(fixed_size);
        let idx = {
            let tree: RedBlackTreeReadOnly<ClaimedSeat> =
                RedBlackTreeReadOnly::new(dyn_bytes, claimed_seats_root_index, NIL);
            tree.lookup_index(&ClaimedSeat::new_empty(*owner, 0, 0))
        };
        assert_ne!(idx, NIL, "no seat for {}", owner);

        {
            let node = get_mut_helper_seat(dyn_bytes, idx);
            let seat = node.get_mut_value();
            if is_debt {
                seat.debt_withdrawable_shares = seat
                    .debt_withdrawable_shares
                    .checked_add(shares)
                    .expect("share overflow");
            } else {
                seat.collateral_withdrawable_shares = seat
                    .collateral_withdrawable_shares
                    .checked_add(shares)
                    .expect("share overflow");
            }
        }

        let lamports = solana_sdk::rent::Rent::default().minimum_balance(data.len());
        let acc = Account {
            lamports,
            data,
            owner: ydelta::ID,
            executable: false,
            rent_epoch: 0,
        };
        self.context
            .borrow_mut()
            .set_account(&self.market.pubkey(), &acc.into());
    }

    // ─────────────────── Vault helpers ───────────────────

    /// Call `create_vault` for the market's debt mint (USDC), then
    /// transfer global_vault_admin to the test's intended admin.
    ///
    /// CreateVault is gated on `GlobalConfig.protocol_admin`, which is
    /// `self.payer` (the fixture installs it during setup). The test
    /// usually wants a different keypair to be the global_vault_admin (so it
    /// can drive vault-internal admin actions). We bridge that with
    /// the standard two-step transfer flow: protocol_admin creates,
    /// transfers to the test's `admin`, who accepts.
    pub async fn create_vault(
        &self,
        admin: &Keypair,
    ) -> Result<(), solana_program_test::BanksClientError> {
        // Step 1: protocol_admin creates the vault (becomes initial
        // global_vault_admin = self.payer).
        let create_ix = create_vault_instruction(
            &mainnet::usdc_mint(),
            &self.payer.pubkey(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &marginfi_mocks::ID,
            &spl_token::id(),
            &spl_token_2022::id(),
        );
        let payer_kp = self.payer.insecure_clone();
        self.process(create_ix, &[&payer_kp]).await?;
        self.refresh_blockhash().await;

        // Step 2: protocol_admin initiates transfer to the test's admin.
        let transfer_ix =
            ydelta::program::instruction_builders::admin_transfer_instructions::transfer_global_vault_admin_instruction(
                &mainnet::usdc_mint(),
                &self.payer.pubkey(),
                &admin.pubkey(),
            );
        let payer_kp = self.payer.insecure_clone();
        self.process(transfer_ix, &[&payer_kp]).await?;
        self.refresh_blockhash().await;

        // Step 3: test's admin accepts.
        let accept_ix =
            ydelta::program::instruction_builders::admin_transfer_instructions::accept_global_vault_admin_instruction(
                &mainnet::usdc_mint(),
                &admin.pubkey(),
            );
        let admin_kp = admin.insecure_clone();
        self.process(accept_ix, &[&admin_kp]).await
    }

    /// Admin creates a risk profile on the USDC vault.
    pub async fn create_risk_profile(
        &self,
        admin: &Keypair,
        profile_id: u8,
        curator: Pubkey,
        max_ltv_bps: u16,
        max_term_seconds: u32,
        allowed_market_max: u8,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = create_risk_profile_instruction(
            &mainnet::usdc_mint(),
            &admin.pubkey(),
            profile_id,
            &curator,
            max_ltv_bps,
            max_term_seconds,
            allowed_market_max,
        );
        let kp = admin.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Depositor calls `global_vault_deposit` on the USDC vault.
    pub async fn global_vault_deposit(
        &self,
        depositor: &Keypair,
        depositor_token: Pubkey,
        profile_id: u8,
        amount_atoms: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = global_vault_deposit_instruction(
            &mainnet::usdc_mint(),
            &depositor.pubkey(),
            &depositor_token,
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &mainnet::usdc_liquidity_vault(),
            &marginfi_mocks::ID,
            profile_id,
            amount_atoms,
        );
        let kp = depositor.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Depositor calls `global_vault_withdraw` on the USDC vault.
    pub async fn global_vault_withdraw(
        &self,
        depositor: &Keypair,
        depositor_token: Pubkey,
        profile_id: u8,
        shares_to_burn: u128,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let bank_lva = mainnet::liquidity_vault_authority(mainnet::usdc_bank());
        let ix = global_vault_withdraw_instruction(
            &mainnet::usdc_mint(),
            &depositor.pubkey(),
            &depositor_token,
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &mainnet::usdc_oracle(),
            &mainnet::usdc_liquidity_vault(),
            &bank_lva,
            &marginfi_mocks::ID,
            profile_id,
            shares_to_burn,
        );
        let kp = depositor.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Admin claim_seat_for_risk_profile on the USDC vault, this market.
    pub async fn claim_seat_for_risk_profile(
        &self,
        admin: &Keypair,
        profile_id: u8,
        max_exposure_atoms: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = claim_seat_for_risk_profile_instruction(
            &mainnet::usdc_mint(),
            &self.market.pubkey(),
            &admin.pubkey(),
            profile_id,
            max_exposure_atoms,
        );
        let kp = admin.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Vault-admin updates a risk_profile's mutable policy fields.
    pub async fn update_risk_profile(
        &self,
        admin: &Keypair,
        profile_id: u8,
        new_max_ltv_bps: Option<u16>,
        new_max_term_seconds: Option<u32>,
        new_allowed_market_max: Option<u8>,
    ) -> Result<(), solana_program_test::BanksClientError> {
        use ydelta::program::instruction_builders::update_risk_profile_instruction::update_risk_profile_instruction;
        let ix = update_risk_profile_instruction(
            &mainnet::usdc_mint(),
            &admin.pubkey(),
            profile_id,
            new_max_ltv_bps,
            new_max_term_seconds,
            new_allowed_market_max,
        );
        let kp = admin.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Variant of `claim_seat_for_risk_profile` that targets an
    /// arbitrary market (e.g. the secondary market created by
    /// `create_second_market`). Used by the multi-market test.
    pub async fn claim_seat_for_risk_profile_in_market(
        &self,
        admin: &Keypair,
        profile_id: u8,
        market_pk: Pubkey,
        max_exposure_atoms: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = claim_seat_for_risk_profile_instruction(
            &mainnet::usdc_mint(),
            &market_pk,
            &admin.pubkey(),
            profile_id,
            max_exposure_atoms,
        );
        let kp = admin.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Variant of `place_order_for_risk_profile` for an arbitrary
    /// market. Used by the multi-market test.
    pub async fn place_order_for_risk_profile_in_market(
        &self,
        curator: &Keypair,
        profile_id: u8,
        market_pk: Pubkey,
        rate_bps: u16,
        term_seconds: u32,
        flags: u8,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = place_order_for_risk_profile_instruction(
            &mainnet::usdc_mint(),
            &market_pk,
            &curator.pubkey(),
            profile_id,
            rate_bps,
            term_seconds,
            flags,
        );
        let kp = curator.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Permissionless cranker — re-stamps every passed market's
    /// `risk_profile_max_ltv_bps` cache from the live risk_profile.
    pub async fn sync_market_seats_for_risk_profile(
        &self,
        payer: &Keypair,
        profile_id: u8,
        markets: &[Pubkey],
    ) -> Result<(), solana_program_test::BanksClientError> {
        use ydelta::program::instruction_builders::sync_market_seats_for_risk_profile_instruction::sync_market_seats_for_risk_profile_instruction;
        let ix = sync_market_seats_for_risk_profile_instruction(
            &payer.pubkey(),
            &mainnet::usdc_mint(),
            profile_id,
            markets,
        );
        let kp = payer.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Curator place_order_for_risk_profile. Risk-profile orders are non-expiring
    /// — only the curator can remove them via cancel_order_for_risk_profile.
    pub async fn place_order_for_risk_profile(
        &self,
        curator: &Keypair,
        profile_id: u8,
        rate_bps: u16,
        term_seconds: u32,
        flags: u8,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = place_order_for_risk_profile_instruction(
            &mainnet::usdc_mint(),
            &self.market.pubkey(),
            &curator.pubkey(),
            profile_id,
            rate_bps,
            term_seconds,
            flags,
        );
        let kp = curator.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Curator-gated `claim_curator_fee`.
    pub async fn claim_curator_fee(
        &self,
        curator: &Keypair,
        curator_token: Pubkey,
        profile_id: u8,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let bank_lva = mainnet::liquidity_vault_authority(mainnet::usdc_bank());
        let ix = claim_curator_fee_instruction(
            &mainnet::usdc_mint(),
            &curator.pubkey(),
            &curator_token,
            &mainnet::usdc_bank(),
            &mainnet::usdc_liquidity_vault(),
            &bank_lva,
            &mainnet::usdc_oracle(),
            &spl_token::id(),
            &marginfi_mocks::ID,
            &mainnet::marginfi_group(),
            profile_id,
        );
        let kp = curator.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Read the per-mint `GlobalVaultFixed` header.
    pub async fn read_vault_fixed(&self) -> ydelta::state::vault::GlobalVaultFixed {
        let (global_vault_pda, _) = ydelta::state::vault::global_vault_pda(&mainnet::usdc_mint());
        let data = self.account_data(global_vault_pda).await;
        let size = std::mem::size_of::<ydelta::state::vault::GlobalVaultFixed>();
        *bytemuck::from_bytes(&data[..size])
    }

    /// Read the vault's `RiskProfile` for the given profile_id.
    pub async fn read_risk_profile(&self, profile_id: u8) -> ydelta::state::vault::RiskProfile {
        use hypertree::{HyperTreeReadOperations, NIL};
        let (global_vault_pda, _) = ydelta::state::vault::global_vault_pda(&mainnet::usdc_mint());
        let data = self.account_data(global_vault_pda).await;
        let size = std::mem::size_of::<ydelta::state::vault::GlobalVaultFixed>();
        let header: &ydelta::state::vault::GlobalVaultFixed = bytemuck::from_bytes(&data[..size]);
        let dynamic = &data[size..];
        let tree = ydelta::state::vault::RiskProfileTreeReadOnly::new(
            dynamic,
            header.risk_profiles_root_index,
            NIL,
        );
        let probe =
            ydelta::state::vault::RiskProfile::new_empty(profile_id, Pubkey::default(), 1, 1, 0);
        let idx = tree.lookup_index(&probe);
        assert_ne!(idx, NIL, "no risk profile {}", profile_id);
        *ydelta::state::vault::get_helper_risk_profile(dynamic, idx).get_value()
    }

    /// Read the trader's seat from the market account.
    pub async fn read_seat(&self, owner: &Pubkey) -> ydelta::state::ClaimedSeat {
        use hypertree::{HyperTreeReadOperations, RedBlackTreeReadOnly, NIL};
        use ydelta::state::market::get_helper_seat;
        use ydelta::state::{ClaimedSeat, MarketFixed};

        let data = self.account_data(self.market.pubkey()).await;
        let fixed_size = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&data[..fixed_size]);
        let dynamic = &data[fixed_size..];
        let tree: RedBlackTreeReadOnly<ClaimedSeat> =
            RedBlackTreeReadOnly::new(dynamic, header.claimed_seats_root_index, NIL);
        let idx = tree.lookup_index(&ClaimedSeat::new_empty(*owner, 0, 0));
        assert_ne!(idx, NIL, "no seat for {}", owner);
        *get_helper_seat(dynamic, idx).get_value()
    }

    /// Read a risk-profile-owned `ClaimedSeat` keyed by
    /// `(global_vault_pubkey, OWNER_KIND_RISK_PROFILE, profile_id)`.
    pub async fn read_vault_seat(
        &self,
        vault: &Pubkey,
        profile_id: u8,
    ) -> ydelta::state::ClaimedSeat {
        use hypertree::{HyperTreeReadOperations, RedBlackTreeReadOnly, NIL};
        use ydelta::state::claimed_seat::OWNER_KIND_RISK_PROFILE;
        use ydelta::state::market::get_helper_seat;
        use ydelta::state::{ClaimedSeat, MarketFixed};

        let data = self.account_data(self.market.pubkey()).await;
        let fixed_size = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&data[..fixed_size]);
        let dynamic = &data[fixed_size..];
        let tree: RedBlackTreeReadOnly<ClaimedSeat> =
            RedBlackTreeReadOnly::new(dynamic, header.claimed_seats_root_index, NIL);
        let probe = ClaimedSeat::new_empty(*vault, OWNER_KIND_RISK_PROFILE, profile_id);
        let idx = tree.lookup_index(&probe);
        assert_ne!(
            idx, NIL,
            "no vault seat for (vault={}, profile_id={})",
            vault, profile_id
        );
        *get_helper_seat(dynamic, idx).get_value()
    }
}

/// Idempotent: ensure `<SBF_OUT_DIR>/marginfi.so` is in place so
/// `add_program("marginfi", ...)` resolves it. Mirrors AdapterFixture's
/// helper but is duplicated here to keep `MarketFixture` self-contained.
fn ensure_marginfi_so_in_sbf_dir() {
    let fixtures_marginfi =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/marginfi.so");
    let sbf_out = match std::env::var("SBF_OUT_DIR") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            panic!("SBF_OUT_DIR not set. Run via `cargo test-sbf` or `scripts/test.sh --sbf`.")
        }
    };
    let target = sbf_out.join("marginfi.so");
    if !target.exists() {
        std::fs::copy(&fixtures_marginfi, &target)
            .unwrap_or_else(|e| panic!("failed to copy marginfi.so into {:?}: {}", sbf_out, e));
    }
}
