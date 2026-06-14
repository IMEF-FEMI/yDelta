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

use hypertree::{HyperTreeReadOperations, HyperTreeValueIteratorTrait, RedBlackTreeReadOnly, NIL};
use solana_program::pubkey::Pubkey;
use solana_program_test::{ProgramTest, ProgramTestContext};
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::{
    account::Account,
    instruction::Instruction,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

use ydelta::program::instruction_builders::{
    claim_curator_fee_instruction::claim_curator_fee_instruction,
    claim_repayment_for_sub_vault_instruction::claim_repayment_for_sub_vault_instruction,
    claim_seat_instruction::claim_seat_instruction,
    create_market_instructions::create_market_instructions,
    create_sub_vault_instruction::{
        create_pool_sub_vault_instruction, create_private_sub_vault_instruction,
    },
    create_vault_instruction::create_vault_instruction,
    deposit_instruction::deposit_instruction,
    global_vault_deposit_instruction::global_vault_deposit_instruction,
    global_vault_withdraw_instruction::global_vault_withdraw_instruction,
    liquidate_loan_instruction::liquidate_loan_instruction,
    place_order_for_sub_vault_instruction::place_order_for_sub_vault_instruction,
    place_order_instruction::place_order_instruction,
    process_matched_loan_instruction::{process_matched_loan_instruction, VaultSettleAddrs},
    repay_instruction::repay_instruction,
    settle_matured_loan_instruction::settle_matured_loan_instruction,
    update_order_for_sub_vault_instruction::update_order_for_sub_vault_instruction,
    update_sub_vault_instruction::update_sub_vault_instruction,
    withdraw_instruction::withdraw_instruction,
};
use ydelta::program::processor::create_market::CreateMarketParams;
use ydelta::state::claimed_seat::OWNER_KIND_SUB_VAULT;
use ydelta::state::market::{
    get_helper_seat, get_mut_helper_seat, MarketFixed, MatchedLoan, MatchedLoanTreeReadOnly,
};
use ydelta::state::vault::{
    global_vault_integration_account_pda, global_vault_pda, global_vault_signer_pda,
    global_vault_staging_pda,
};
use ydelta::state::{ClaimedSeat, OrderType, Side};
use ydelta::validation::{
    get_lender_integration_account_address, get_market_signer_address, get_vault_address,
};

use super::marginfi_fixture::{load_account_from_fixture, mainnet};

pub struct MarketFixture {
    pub context: Rc<RefCell<ProgramTestContext>>,
    pub payer: Keypair,
    pub market: Keypair,
    /// Optional secondary market on the same `(USDC, wSOL)` pair, lazily
    /// stood up by `create_second_market()`. The vault is per-mint, so
    /// a single sub_vault can claim seats in both markets and earn
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
    /// the multi-market sub-vault test that asserts a profile in two
    /// markets earns the SUM of both lender interests. After calling,
    /// `second_market_pubkey()` returns the second market's address.
    pub async fn create_second_market(&self) {
        let market2 = Keypair::new();
        let payer = self.payer.insecure_clone();
        let params = CreateMarketParams::default();
        let ixs = create_market_instructions(
            &market2.pubkey(),
            &mainnet::usdc_mint(),
            &mainnet::wsol_mint(),
            &payer.pubkey(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &mainnet::sol_bank(),
            &marginfi_mocks::ID,
            &params,
        )
        .unwrap();
        let m2_kp = market2.insecure_clone();
        self.process_ixs(&ixs, &[&m2_kp]).await.unwrap();
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
        let params = CreateMarketParams::default();
        let ixs = create_market_instructions(
            &self.market.pubkey(),
            &mainnet::usdc_mint(),
            &mainnet::wsol_mint(),
            &payer.pubkey(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &mainnet::sol_bank(),
            &marginfi_mocks::ID,
            &params,
        )
        .unwrap();
        let market_kp = self.market.insecure_clone();
        self.process_ixs(&ixs, &[&market_kp]).await.unwrap();
    }

    /// Returns the lender-side marginfi-account PDA at
    /// `[b"marginfi_account", market.key]`. `marginfi_account_pubkey` is
    /// kept as an alias for the same address.
    pub fn lender_marginfi_account_pubkey(&self) -> Pubkey {
        ydelta::validation::get_lender_integration_account_address(&self.market.pubkey()).0
    }

    /// Returns the borrower-side marginfi-account PDA at
    /// `[b"borrower_marginfi_account", market.key]`. Holds collateral as
    /// asset on the collateral bank + P2Pool liability on the debt bank.
    pub fn borrower_marginfi_account_pubkey(&self) -> Pubkey {
        ydelta::validation::get_borrower_integration_account_address(&self.market.pubkey()).0
    }

    /// Alias for `lender_marginfi_account_pubkey`.
    pub fn marginfi_account_pubkey(&self) -> Pubkey {
        self.lender_marginfi_account_pubkey()
    }

    /// Return the synthetic debt-mint token account address for `signer`,
    /// creating it on first call. Used by `place_order` so the loader's
    /// owner/mint validation on `borrower_debt_token` passes.
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
        // Several yDelta instructions are CPI-heavy (marginfi withdraw +
        // deposit + token transfers) and legitimately run at 90-100%+ of
        // the default 200k per-instruction CU budget. Prepend a raised
        // limit so test transactions have deterministic headroom — the
        // same `set_compute_unit_limit` a real client attaches for these
        // instructions.
        let cu = ComputeBudgetInstruction::set_compute_unit_limit(600_000);
        self.process_ixs(&[cu, ix], extra_signers).await
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
        let bh = client.get_new_latest_blockhash().await.unwrap();
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
        self.withdraw_inner(signer, trader_token, is_debt, amount_atoms, false)
            .await
    }

    /// Build + send a ydelta `Withdraw` ix with `withdraw_all = true`,
    /// draining the seat's entire withdrawable balance on the side.
    pub async fn withdraw_all(
        &self,
        signer: &Keypair,
        trader_token: Pubkey,
        is_debt: bool,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.withdraw_inner(signer, trader_token, is_debt, 0, true)
            .await
    }

    async fn withdraw_inner(
        &self,
        signer: &Keypair,
        trader_token: Pubkey,
        is_debt: bool,
        amount_atoms: u64,
        withdraw_all: bool,
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
            withdraw_all,
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
            ydelta::state::market_helpers::RESIDUAL_MODE_DROP,
        )
        .await
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
            rate_bps,
            term_seconds,
            principal_atoms,
            collateral_atoms,
            0, // ltv_buffer_bps (off)
            flags, // residual_mode
            0,     // last_valid_unix_ts: rested bids never expire by default
            None,
        );
        let _ = (side, order_type);
        let kp = signer.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Variant of `place_order_with_flags` that also exposes
    /// `ltv_buffer_bps` (the borrower-set origination-LTV buffer).
    /// Mirrors `place_order_with_flags` exactly; only `ltv_buffer_bps`
    /// is threaded through to the builder.
    #[allow(clippy::too_many_arguments)]
    pub async fn place_order_with_buffer(
        &self,
        signer: &Keypair,
        side: Side,
        order_type: OrderType,
        rate_bps: u16,
        term_seconds: u32,
        principal_atoms: u64,
        collateral_atoms: u64,
        ltv_buffer_bps: u16,
        flags: u8,
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
            rate_bps,
            term_seconds,
            principal_atoms,
            collateral_atoms,
            ltv_buffer_bps,
            flags, // residual_mode
            0,     // last_valid_unix_ts: rested bids never expire by default
            None,
        );
        let _ = (side, order_type);
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
            rate_bps,
            term_seconds,
            principal_atoms,
            collateral_atoms,
            0, // ltv_buffer_bps (off)
            flags, // residual_mode
            0,     // last_valid_unix_ts: rested bids never expire by default
            None,
        );
        let _ = (side, order_type);
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
        let loan = self.read_loan_in(market_pk, sequence).await;
        let cranker_refund = loan.created_by;
        let gv_pk = global_vault_pda(&mainnet::usdc_bank()).0;
        let global_vault: Option<&Pubkey> = if loan.loan_type == ydelta::state::loan::LoanType::Fixed as u8 {
            Some(&gv_pk)
        } else {
            None
        };
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
            &cranker_refund,
            global_vault,
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

    /// Read the on-disk `ClaimedSeat` body at the given index.
    pub async fn read_seat_data(&self, index: hypertree::DataIndex) -> ydelta::state::ClaimedSeat {
        let data = self.account_data(self.market.pubkey()).await;
        let fixed_size = std::mem::size_of::<ydelta::state::MarketFixed>();
        let dynamic = &data[fixed_size..];
        *ydelta::state::market::get_helper_seat(dynamic, index).get_value()
    }

    /// Look up a trader's seat index. Returns `NIL` if not claimed.
    pub async fn read_seat_index(&self, owner: &Pubkey) -> hypertree::DataIndex {
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

    /// Cranker promote for a sub-vault-funded MatchedLoan. Threads
    /// the trailing vault-settle accounts so `do_vault_settle` can run
    /// the 3-CPI atom migration (vault.integration → global_vault_staging →
    /// market_debt_vault → market.lender_integration_account).
    pub async fn crank_matched_loan_for_sub_vault(
        &self,
        sequence: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.refresh_oracle_freshness().await;
        let market_pk = self.market.pubkey();
        let (gv, _) = global_vault_pda(&mainnet::usdc_bank());
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

    /// Permissionless cranker — drains a fully-repaid sub-vault loan
    /// from `market.lender_integration_account` back into the
    /// GlobalVault's marginfi account, decrements per-market exposure
    /// usage, sweeps protocol-fee shares onto the market accumulator,
    /// Stateless seat sweeper — pulls all pending claim shares from the
    /// vault's sub-vault market seat into the vault's integration
    /// account. Per the repay/claim split, this NEVER touches the loan
    /// PDA. Caller is the signer; pass any keypair.
    ///
    /// Note: signature changed from `sequence: u64, cranker_refund` to
    /// `sub_vault_id: u16`. The loan PDA is closed by repay/liquidate/
    /// settle now, so the per-loan address is irrelevant — claim keys
    /// on the seat lookup by `(market, vault, sub_vault_id)`.
    pub async fn claim_repayment_for_sub_vault(
        &self,
        cranker: &Keypair,
        sub_vault_id: u16,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.refresh_oracle_freshness().await;
        let market_pk = self.market.pubkey();
        let (gv, _) = global_vault_pda(&mainnet::usdc_bank());
        let (lender_mfi, _) = get_lender_integration_account_address(&market_pk);
        let (debt_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::usdc_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let ix = claim_repayment_for_sub_vault_instruction(
            &cranker.pubkey(),
            &market_pk,
            sub_vault_id,
            &gv,
            &mainnet::usdc_mint(),
            &mainnet::usdc_bank(),
            &mainnet::usdc_liquidity_vault(),
            &debt_bank_lva,
            &[mainnet::usdc_oracle()],
            &lender_mfi,
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &marginfi_mocks::ID,
        );
        // Two marginfi CPIs (withdraw, deposit) + SPL transfer push the
        // ix above the 200k default; bump to 600k for headroom.
        let cu = ComputeBudgetInstruction::set_compute_unit_limit(600_000);
        let kp = cranker.insecure_clone();
        self.process_ixs(&[cu, ix], &[&kp]).await
    }

    /// Variant of `crank_matched_loan_for_sub_vault` for an
    /// arbitrary market. Used by the multi-market test.
    pub async fn crank_matched_loan_for_sub_vault_in_market(
        &self,
        market_pk: Pubkey,
        sequence: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.refresh_oracle_freshness().await;
        let (gv, _) = global_vault_pda(&mainnet::usdc_bank());
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

    /// Variant of `claim_repayment_for_sub_vault` for an arbitrary
    /// market.
    pub async fn claim_repayment_for_sub_vault_in_market(
        &self,
        cranker: &Keypair,
        market_pk: Pubkey,
        sub_vault_id: u16,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.refresh_oracle_freshness().await;
        let (gv, _) = global_vault_pda(&mainnet::usdc_bank());
        let (lender_mfi, _) = get_lender_integration_account_address(&market_pk);
        let (debt_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::usdc_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let ix = claim_repayment_for_sub_vault_instruction(
            &cranker.pubkey(),
            &market_pk,
            sub_vault_id,
            &gv,
            &mainnet::usdc_mint(),
            &mainnet::usdc_bank(),
            &mainnet::usdc_liquidity_vault(),
            &debt_bank_lva,
            &[mainnet::usdc_oracle()],
            &lender_mfi,
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &marginfi_mocks::ID,
        );
        // 600k for headroom.
        let cu = ComputeBudgetInstruction::set_compute_unit_limit(600_000);
        let kp = cranker.insecure_clone();
        self.process_ixs(&[cu, ix], &[&kp]).await
    }
    /// Fast-forward the test ledger's `Clock` so loans can move past
    /// their `matures_at_unix` boundary. Sets the sysvar directly via
    /// `set_sysvar`. Subsequent ixs see `now = new_unix_ts` until
    /// `set_sysvar` is called again or the test ends.
    pub async fn set_clock_unix_timestamp(&self, new_unix_ts: i64) {
        let ctx: RefMut<ProgramTestContext> = self.context.borrow_mut();
        let mut clock: solana_sdk::clock::Clock = ctx.banks_client.get_sysvar().await.unwrap();
        assert!(
            new_unix_ts >= clock.unix_timestamp,
            "set_clock_unix_timestamp: rewind from {} to {} forbidden (T-12)",
            clock.unix_timestamp,
            new_unix_ts,
        );
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
        let loan = self.read_loan(sequence).await;
        let cranker_refund = loan.created_by;
        let gv_pk = global_vault_pda(&mainnet::usdc_bank()).0;
        let global_vault: Option<&Pubkey> = if loan.loan_type == ydelta::state::loan::LoanType::Fixed as u8 {
            Some(&gv_pk)
        } else {
            None
        };
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
            &cranker_refund,
            global_vault,
        );
        let kp = borrower.insecure_clone();
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
        let loan = self.read_loan(sequence).await;
        let cranker_refund = loan.created_by;
        let (debt_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::usdc_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let (coll_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::sol_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let _ = debt_bank_lva;
        let gv_pk = global_vault_pda(&mainnet::usdc_bank()).0;
        let global_vault: Option<&Pubkey> = if loan.loan_type == ydelta::state::loan::LoanType::Fixed as u8 {
            Some(&gv_pk)
        } else {
            None
        };
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
            &cranker_refund,
            global_vault,
        );
        let kp = liquidator.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Run `SettleMaturedLoan`. Permissionless cranker action
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
        let loan = self.read_loan(sequence).await;
        let cranker_refund = loan.created_by;
        let (_debt_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::usdc_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let (coll_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::sol_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let gv_pk = global_vault_pda(&mainnet::usdc_bank()).0;
        let global_vault: Option<&Pubkey> = if loan.loan_type == ydelta::state::loan::LoanType::Fixed as u8 {
            Some(&gv_pk)
        } else {
            None
        };
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
            &cranker_refund,
            global_vault,
        );
        // Settle does marginfi.withdraw (collateral) + deposit (debt
        // for repay) + repay + transfers + seat updates, plus the
        // group-binding decodes in the loader — pushes well above the
        // 200k default ix budget.
        let cu = ComputeBudgetInstruction::set_compute_unit_limit(600_000);
        let kp = liquidator.insecure_clone();
        self.process_ixs(&[cu, ix], &[&kp]).await
    }

    /// Run `ConvertP2PoolToFixed`. `loan_sequence` identifies
    /// the P2Pool loan PDA. The ix walks the asks tree and crosses every
    /// compatible ask whose `rate_bps <= max_acceptable_rate_bps`
    /// AND `term_seconds >= (loan.matures_at - now)`; each cross emits
    /// a Fixed MatchedLoan (cranker promotes). The rent-refund target
    /// is the loan's `created_by` (required by the loader); the helper
    /// resolves it from the loan account automatically.
    pub async fn convert_p2pool_to_fixed(
        &self,
        borrower: &Keypair,
        loan_sequence: u64,
        max_acceptable_rate_bps: u16,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let (debt_bank_lva, _) = solana_program::pubkey::Pubkey::find_program_address(
            &[b"liquidity_vault_auth", mainnet::usdc_bank().as_ref()],
            &marginfi_mocks::ID,
        );
        let cranker_refund = self.read_loan(loan_sequence).await.created_by;
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
            0, // ltv_buffer_bps (off)
            &cranker_refund,
        );
        let kp = borrower.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Overwrite the Switchboard CurrentResult.value (the price in
    /// fp18 decimal scale) on a Switchboard-pull oracle. Used by
    /// liquidation tests to crash the collateral oracle below the
    /// maint-LTV threshold without rebuilding the fixture.
    /// Scale the USDC bank's `total_liability_shares` by
    /// `num / den` — pushes utilization (and therefore the live lending
    /// APR / v1 D5 ask floor) up or down for floor tests.
    pub async fn scale_usdc_bank_liabilities(&self, num: u64, den: u64) {
        // Bank layout: 8-byte discriminator + body;
        // total_liability_shares (I80F48) at body offset 248.
        const OFFSET: usize = 8 + 248;
        let address = mainnet::usdc_bank();
        let mut ctx: RefMut<ProgramTestContext> = self.context.borrow_mut();
        let mut account = ctx
            .banks_client
            .get_account(address)
            .await
            .unwrap()
            .expect("usdc bank fixture loaded");
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&account.data[OFFSET..OFFSET + 16]);
        let v = i128::from_le_bytes(buf);
        let scaled = v / (den as i128) * (num as i128);
        account.data[OFFSET..OFFSET + 16].copy_from_slice(&scaled.to_le_bytes());
        ctx.set_account(&address, &account.into());
    }

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

    /// Sum `collateral_atoms` over every `MatchedLoan` resting in the
    /// market's matched-loan tree. Used to verify a multi-cross bid
    /// leaves zero collateral dust frozen — the sum of per-loan
    /// collateral must equal the bid's posted collateral.
    pub async fn sum_matched_loan_collateral(&self) -> u64 {
        let data = self.account_data(self.market.pubkey()).await;
        let fixed_size = std::mem::size_of::<MarketFixed>();
        let root =
            bytemuck::from_bytes::<MarketFixed>(&data[..fixed_size]).matched_loans_root_index;
        let dynamic = &data[fixed_size..];
        let tree = MatchedLoanTreeReadOnly::new(dynamic, root, NIL);
        let mut total: u64 = 0;
        for (_idx, node) in tree.iter::<MatchedLoan>() {
            total = total.checked_add(node.collateral_atoms).unwrap();
        }
        total
    }

    /// Read the on-disk LoanFixed at the per-(market, sequence) PDA.
    pub async fn read_loan(&self, sequence: u64) -> ydelta::state::loan::LoanFixed {
        let (loan_addr, _) = ydelta::state::loan::loan_pda(&self.market.pubkey(), sequence);
        let data = self.account_data(loan_addr).await;
        *bytemuck::from_bytes::<ydelta::state::loan::LoanFixed>(
            &data[..std::mem::size_of::<ydelta::state::loan::LoanFixed>()],
        )
    }

    /// True iff the loan PDA at sequence has been closed (account no longer
    /// exists). Use after `repay(full_repay=true)`, `liquidate_loan`, or
    /// `settle_matured_loan` to assert close-on-completion semantics
    /// instead of reading the body (which would panic on a closed PDA).
    pub async fn loan_account_is_closed(&self, sequence: u64) -> bool {
        let (loan_addr, _) =
            ydelta::state::loan::loan_pda(&self.market.pubkey(), sequence);
        let ctx = self.context.borrow_mut();
        ctx.banks_client
            .get_account(loan_addr)
            .await
            .unwrap()
            .is_none()
    }

    /// Test backdoor: bump a seat's `*_withdrawable_shares` directly,
    /// bypassing the deposit ix. Used by tests that need pre-existing
    /// seat balance to drive matching/encumbrance flows but don't want
    /// to push the actual atoms through marginfi (e.g. for sides where
    /// the bank's interest math overflows on synthetic test deposits).
    pub async fn seed_seat_shares(&self, owner: &Pubkey, shares: u128, is_debt: bool) {
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
            // Scale to I80F48 — see the matching helper in fixture.rs.
            let scaled = shares
                .checked_shl(48)
                .expect("seed shares too large for I80F48");
            if is_debt {
                seat.debt_withdrawable_shares = seat
                    .debt_withdrawable_shares
                    .checked_add(scaled)
                    .expect("share overflow");
            } else {
                seat.collateral_withdrawable_shares = seat
                    .collateral_withdrawable_shares
                    .checked_add(scaled)
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

    /// Force a seat into the collateral shape where a live loan's
    /// collateral sits in `collateral_withdrawable_shares` with
    /// `collateral_encumbered_shares == 0`: move ALL encumbered shares
    /// into withdrawable. Used to prove close-time `release_loan_collateral`
    /// is a safe no-op for such a seat (no double-credit, no stranding).
    pub async fn legacy_collateral_to_withdrawable(&self, owner: &Pubkey) {
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
            seat.collateral_withdrawable_shares = seat
                .collateral_withdrawable_shares
                .checked_add(seat.collateral_encumbered_shares)
                .expect("share overflow");
            seat.collateral_encumbered_shares = 0;
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
            &mainnet::usdc_bank(),
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
                &mainnet::usdc_bank(),
                &self.payer.pubkey(),
                &admin.pubkey(),
            );
        let payer_kp = self.payer.insecure_clone();
        self.process(transfer_ix, &[&payer_kp]).await?;
        self.refresh_blockhash().await;

        // Step 3: test's admin accepts.
        let accept_ix =
            ydelta::program::instruction_builders::admin_transfer_instructions::accept_global_vault_admin_instruction(
                &mainnet::usdc_bank(),
                &admin.pubkey(),
            );
        let admin_kp = admin.insecure_clone();
        self.process(accept_ix, &[&admin_kp]).await
    }

    /// Vault-admin toggles `GlobalVaultFixed.is_paused`.
    pub async fn set_vault_pause(
        &self,
        admin: &Keypair,
        paused: bool,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let (vault_pk, _) = ydelta::state::vault::global_vault_pda(&mainnet::usdc_bank());
        let ix =
            ydelta::program::instruction_builders::set_vault_pause_instruction::set_vault_pause_instruction(
                &vault_pk,
                &admin.pubkey(),
                paused,
            );
        let kp = admin.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Admin creates a sub-vault on the USDC vault. The
    /// `sub_vault_id` is assigned by the program (vault's monotonic
    /// `next_sub_vault_id` counter); off-chain tests that need to know
    /// which id was assigned should read the
    /// `SubVaultCreatedLog` event or pre-snapshot the counter.
    /// v1 D3: Pool creation is PROTOCOL-admin gated, so this signs with
    /// `self.payer` (the fixture's protocol admin). The `admin` param is
    /// retained for call-site compatibility but no longer signs.
    /// Liquidation LTV defaults to `max_ltv + 1_000` (capped at 10_000)
    /// and the curator fee to 0; use `create_pool_sub_vault_full` when a
    /// test needs explicit values.
    pub async fn create_sub_vault(
        &self,
        _admin: &Keypair,
        curator: Pubkey,
        max_ltv_bps: Option<u16>,
        max_term_seconds: u32,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ltv = max_ltv_bps.expect("v1: max_ltv_bps is required");
        self.create_pool_sub_vault_full(
            curator,
            /*spread_bps=*/ 0,
            ltv,
            /*liquidation_ltv_bps=*/ ltv.saturating_add(1_000).min(10_000),
            max_term_seconds,
            /*curator_fee_bps=*/ 0,
        )
        .await
    }

    /// Full-parameter Pool creation, signed by the protocol admin
    /// (`self.payer`).
    pub async fn create_pool_sub_vault_full(
        &self,
        curator: Pubkey,
        spread_bps: u16,
        max_ltv_bps: u16,
        liquidation_ltv_bps: u16,
        max_term_seconds: u32,
        curator_fee_bps: u16,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = create_pool_sub_vault_instruction(
            &mainnet::usdc_bank(),
            &self.payer.pubkey(),
            &curator,
            spread_bps,
            max_ltv_bps,
            liquidation_ltv_bps,
            max_term_seconds,
            curator_fee_bps,
        );
        let kp = self.payer.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Permissionless Private sub-vault creation; `owner` signs and
    /// becomes curator + sole depositor (v1 D2).
    pub async fn create_private_sub_vault(
        &self,
        owner: &Keypair,
        max_ltv_bps: u16,
        liquidation_ltv_bps: u16,
        max_term_seconds: u32,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = create_private_sub_vault_instruction(
            &mainnet::usdc_bank(),
            &owner.pubkey(),
            /*spread_bps=*/ 0,
            max_ltv_bps,
            liquidation_ltv_bps,
            max_term_seconds,
        );
        let kp = owner.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Depositor calls `global_vault_deposit` on the USDC vault.
    pub async fn global_vault_deposit(
        &self,
        depositor: &Keypair,
        depositor_token: Pubkey,
        sub_vault_id: u16,
        amount_atoms: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = global_vault_deposit_instruction(
            &mainnet::usdc_bank(),
            &mainnet::usdc_mint(),
            &depositor.pubkey(),
            &depositor_token,
            &spl_token::id(),
            &mainnet::marginfi_group(),
            &mainnet::usdc_bank(),
            &mainnet::usdc_liquidity_vault(),
            &marginfi_mocks::ID,
            sub_vault_id,
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
        sub_vault_id: u16,
        shares_to_burn: u128,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let bank_lva = mainnet::liquidity_vault_authority(mainnet::usdc_bank());
        let ix = global_vault_withdraw_instruction(
            &mainnet::usdc_bank(),
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
            sub_vault_id,
            shares_to_burn,
        );
        let kp = depositor.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// CURATOR updates a sub_vault's mutable policy fields (v1 D15).
    /// The resulting LTV pair must satisfy the liq-gap invariant; pass
    /// `new_liquidation_ltv_bps` when raising `new_max_ltv_bps`.
    pub async fn update_sub_vault(
        &self,
        curator: &Keypair,
        sub_vault_id: u16,
        new_max_ltv_bps: Option<u16>,
        new_max_term_seconds: Option<u32>,
    ) -> Result<(), solana_program_test::BanksClientError> {
        // Default companion liq override keeps the pair valid for tests
        // that only move the origination cap.
        let new_liq = new_max_ltv_bps.map(|v| v.saturating_add(1_000).min(10_000));
        let ix = update_sub_vault_instruction(
            &mainnet::usdc_bank(),
            &curator.pubkey(),
            sub_vault_id,
            new_max_ltv_bps,
            new_liq,
            new_max_term_seconds,
            /*new_spread_bps=*/ None,
        );
        let kp = curator.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Variant of `place_order_for_sub_vault` for an arbitrary
    /// market. Used by the multi-market test.
    /// v1 D4: rate/term are DERIVED on-chain (bank APR + sub-vault
    /// spread; sub-vault max term). The `_rate_bps`/`_term_seconds`
    /// params are retained for call-site compatibility but ignored —
    /// set the spread/term at sub-vault creation instead.
    pub async fn place_order_for_sub_vault_in_market(
        &self,
        curator: &Keypair,
        sub_vault_id: u16,
        market_pk: Pubkey,
        _rate_bps: u16,
        _term_seconds: u32,
        flags: u8,
    ) -> Result<(), solana_program_test::BanksClientError> {
        // The v1 D7 take path reads both oracles at placement — refresh
        // their timestamps so the staleness gate passes.
        self.refresh_oracle_freshness().await;
        let ix = place_order_for_sub_vault_instruction(
            &mainnet::usdc_bank(),
            &market_pk,
            &self.payer.pubkey(),
            &curator.pubkey(),
            &mainnet::usdc_bank(),
            &mainnet::marginfi_group(),
            &mainnet::sol_bank(),
            &[mainnet::usdc_oracle()],
            &[mainnet::sol_oracle()],
            sub_vault_id,
            flags,
        );
        let kp = curator.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Curator place_order_for_sub_vault. Risk-profile orders are non-expiring
    /// — only the curator can remove them via cancel_order_for_sub_vault.
    /// `self.payer` is used as the ix's fee_payer (covers tx fee + any
    /// rent for vault node-block expansion); curator only signs.
    pub async fn place_order_for_sub_vault(
        &self,
        curator: &Keypair,
        sub_vault_id: u16,
        rate_bps: u16,
        term_seconds: u32,
        flags: u8,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.place_order_for_sub_vault_in_market(
            curator,
            sub_vault_id,
            self.market.pubkey(),
            rate_bps,
            term_seconds,
            flags,
        )
        .await
    }

    /// Permissionless `MatchCrank` (v1 D7/D8) on the fixture market.
    pub async fn match_crank(
        &self,
        cranker: &Keypair,
        max_fills: u32,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.refresh_oracle_freshness().await;
        let ix = ydelta::program::instruction_builders::match_crank_instruction::match_crank_instruction(
            &mainnet::usdc_bank(),
            &self.market.pubkey(),
            &cranker.pubkey(),
            &mainnet::usdc_bank(),
            &mainnet::marginfi_group(),
            &mainnet::sol_bank(),
            &[mainnet::usdc_oracle()],
            &[mainnet::sol_oracle()],
            max_fills,
        );
        let kp = cranker.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Curator's parameterless re-sync of their resting ask on the
    /// fixture market (v1 D4): cancel-and-replace at the live bank APR
    /// + sub-vault spread / max term.
    pub async fn update_order_for_sub_vault(
        &self,
        curator: &Keypair,
        sub_vault_id: u16,
    ) -> Result<(), solana_program_test::BanksClientError> {
        self.refresh_oracle_freshness().await;
        let ix = update_order_for_sub_vault_instruction(
            &mainnet::usdc_bank(),
            &self.market.pubkey(),
            &self.payer.pubkey(),
            &curator.pubkey(),
            &mainnet::usdc_bank(),
            &mainnet::marginfi_group(),
            &mainnet::sol_bank(),
            &[mainnet::usdc_oracle()],
            &[mainnet::sol_oracle()],
            sub_vault_id,
            /*new_flags=*/ 0,
        );
        let kp = curator.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Read the best (lowest-rate) resting ask's stored rate from the
    /// market's bookside, or None when the book is empty. Tests use this
    /// to derive crossing bids under the v1 D4 spread model.
    pub async fn best_ask_rate_bps(&self) -> Option<u16> {
        let data = self.account_data(self.market.pubkey()).await;
        let fixed_size = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&data[..fixed_size]);
        let dynamic = &data[fixed_size..];
        let tree: RedBlackTreeReadOnly<ydelta::state::RestingOrder> =
            RedBlackTreeReadOnly::new(dynamic, header.asks_root_index, header.asks_best_index);
        let best = tree.get_max_index();
        if best == NIL {
            return None;
        }
        Some(
            ydelta::state::market::get_helper_order(dynamic, best)
                .get_value()
                .rate_bps,
        )
    }

    /// Convenience helper for the quote-only lender flow. Sets up a
    /// vault sub-vault that rests an unbounded ask on the market:
    ///   1. `create_vault` (idempotent — skipped if already created)
    ///   2. `create_sub_vault`
    ///   3. `global_vault_deposit` (funds the profile's idle pool)
    ///   4. `place_order_for_sub_vault` (unbounded ask at rate/term)
    ///
    /// The depositor's debt-token account is created/funded with the
    /// deposit amount automatically. Returns the depositor's token
    /// account pubkey for any post-flow assertions.
    #[allow(clippy::too_many_arguments)]
    pub async fn provide_vault_liquidity(
        &self,
        admin: &Keypair,
        depositor: &Keypair,
        curator: &Keypair,
        sub_vault_id: u16,
        max_ltv_bps: Option<u16>,
        rate_bps: u16,
        term_seconds: u32,
        deposit_atoms: u64,
    ) -> Pubkey {
        let depositor_token = self.signer_debt_token(&depositor.pubkey());
        self.put_token_account(
            depositor_token,
            mainnet::usdc_mint(),
            depositor.pubkey(),
            deposit_atoms.saturating_mul(2).max(deposit_atoms),
        );
        self.refresh_blockhash().await;
        // create_vault is once-per-mint; subsequent calls fail because
        // the PDA already exists — tolerate that so this helper is
        // safe to call after an explicit create_vault.
        let _ = self.create_vault(admin).await;
        self.refresh_blockhash().await;
        // `sub_vault_id` is now assigned by the program; the caller's
        // expectation is preserved by the monotonic counter — Nth
        // create on a fresh vault returns id N-1. Downstream calls in
        // this helper use the caller's `sub_vault_id`; if it ever
        // disagrees with the on-chain assignment the
        // `global_vault_deposit` / `place_order_for_sub_vault` calls
        // below will fail with `SubVaultNotFound`, surfacing the
        // mismatch loudly.
        // v1 D4: the old per-order `rate_bps` is the sub-vault's SPREAD
        // over the live bank APR, set at creation.
        let _ = admin;
        let ltv = max_ltv_bps.expect("v1: max_ltv_bps is required");
        self.create_pool_sub_vault_full(
            curator.pubkey(),
            /*spread_bps=*/ rate_bps,
            ltv,
            /*liquidation_ltv_bps=*/ ltv.saturating_add(1_000).min(10_000),
            term_seconds,
            /*curator_fee_bps=*/ 0,
        )
        .await
        .expect("create_pool_sub_vault");
        self.refresh_blockhash().await;
        self.global_vault_deposit(depositor, depositor_token, sub_vault_id, deposit_atoms)
            .await
            .expect("global_vault_deposit");
        self.refresh_blockhash().await;
        self.place_order_for_sub_vault(curator, sub_vault_id, rate_bps, term_seconds, 0)
            .await
            .expect("place_order_for_sub_vault");
        depositor_token
    }

    /// Curator-gated `claim_curator_fee`.
    pub async fn claim_curator_fee(
        &self,
        curator: &Keypair,
        curator_token: Pubkey,
        sub_vault_id: u16,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let bank_lva = mainnet::liquidity_vault_authority(mainnet::usdc_bank());
        let ix = claim_curator_fee_instruction(
            &mainnet::usdc_bank(),
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
            sub_vault_id,
        );
        let kp = curator.insecure_clone();
        self.process(ix, &[&kp]).await
    }

    /// Read the per-mint `GlobalVaultFixed` header.
    pub async fn read_vault_fixed(&self) -> ydelta::state::vault::GlobalVaultFixed {
        let (global_vault_pda, _) = ydelta::state::vault::global_vault_pda(&mainnet::usdc_bank());
        let data = self.account_data(global_vault_pda).await;
        let size = std::mem::size_of::<ydelta::state::vault::GlobalVaultFixed>();
        *bytemuck::from_bytes(&data[..size])
    }

    /// Read the vault's `SubVault` for the given sub_vault_id.
    pub async fn read_sub_vault(&self, sub_vault_id: u16) -> ydelta::state::vault::SubVault {
        let (global_vault_pda, _) = ydelta::state::vault::global_vault_pda(&mainnet::usdc_bank());
        let data = self.account_data(global_vault_pda).await;
        let size = std::mem::size_of::<ydelta::state::vault::GlobalVaultFixed>();
        let header: &ydelta::state::vault::GlobalVaultFixed = bytemuck::from_bytes(&data[..size]);
        let dynamic = &data[size..];
        let tree = ydelta::state::vault::SubVaultTreeReadOnly::new(
            dynamic,
            header.sub_vaults_root_index,
            NIL,
        );
        let probe =
            ydelta::state::vault::SubVault::new_empty(sub_vault_id, Pubkey::default(), 1, 1);
        let idx = tree.lookup_index(&probe);
        assert_ne!(idx, NIL, "no sub-vault {}", sub_vault_id);
        *ydelta::state::vault::get_helper_sub_vault(dynamic, idx).get_value()
    }

    /// Read the trader's seat from the market account.
    pub async fn read_seat(&self, owner: &Pubkey) -> ydelta::state::ClaimedSeat {
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

    /// Read a sub-vault-owned `ClaimedSeat` keyed by
    /// `(global_vault_pubkey, OWNER_KIND_SUB_VAULT, sub_vault_id)`.
    pub async fn read_vault_seat(
        &self,
        vault: &Pubkey,
        sub_vault_id: u16,
    ) -> ydelta::state::ClaimedSeat {
        let data = self.account_data(self.market.pubkey()).await;
        let fixed_size = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&data[..fixed_size]);
        let dynamic = &data[fixed_size..];
        let tree: RedBlackTreeReadOnly<ClaimedSeat> =
            RedBlackTreeReadOnly::new(dynamic, header.claimed_seats_root_index, NIL);
        let probe = ClaimedSeat::new_empty(*vault, OWNER_KIND_SUB_VAULT, sub_vault_id);
        let idx = tree.lookup_index(&probe);
        assert_ne!(
            idx, NIL,
            "no vault seat for (vault={}, sub_vault_id={})",
            vault, sub_vault_id
        );
        *get_helper_seat(dynamic, idx).get_value()
    }

    // ─────────────────── Invariant helpers ───────────────────

    /// Assert `LoanFixed` satisfies the conservation identity at the
    /// given loan sequence:
    ///   `outstanding_debt + principal_retired == lender_claimable +
    ///    accumulated_protocol_fee + accumulated_curator_fee`.
    /// Pulled out as a fixture helper so any state-mutating test can
    /// cheaply re-check it after a repay/liquidate/settle event.
    ///
    /// The identity is enforced via `apply_partial_resolution` on
    /// Fixed-type loans only. P2Pool loans use `outstanding_debt_atoms`
    /// as a non-canonical mirror (the canonical liability lives on the
    /// borrower's marginfi-account); they intentionally skip the
    /// principal-retired bookkeeping. This helper short-circuits for
    /// P2Pool — callers asserting the identity on P2Pool loans is a
    /// misuse, not a production bug.
    pub async fn assert_loan_conservation_holds(&self, sequence: u64) {
        // Under the repay/claim split, full repay closes the loan PDA;
        // conservation is meaningful only on live (partial-repaid or
        // Active) loans. Treat closed = trivially-conserved no-op so
        // callers can chain "assert conservation" after both partial and
        // full repay without branching.
        if self.loan_account_is_closed(sequence).await {
            return;
        }
        let loan = self.read_loan(sequence).await;
        if loan.loan_type == ydelta::state::loan::LoanType::P2Pool as u8 {
            // P2Pool: outstanding_debt_atoms is decorative; canonical
            // liability lives on the borrower marginfi-account. Skip.
            return;
        }
        let claim_side: u128 = (loan.lender_claimable_atoms as u128)
            + (loan.accumulated_protocol_fee_atoms as u128)
            + (loan.accumulated_curator_fee_atoms as u128);
        let owed_side: u128 =
            (loan.outstanding_debt_atoms as u128) + (loan.principal_retired_atoms as u128);
        assert_eq!(
            claim_side,
            owed_side,
            "loan_conservation violated on seq={}: \
             outstanding({}) + retired({}) = {} != \
             lender_claimable({}) + protocol_fee({}) + curator_fee({}) = {}",
            sequence,
            loan.outstanding_debt_atoms,
            loan.principal_retired_atoms,
            owed_side,
            loan.lender_claimable_atoms,
            loan.accumulated_protocol_fee_atoms,
            loan.accumulated_curator_fee_atoms,
            claim_side,
        );
    }

    /// Assert the vault-idle invariant on a given sub-vault:
    ///   `total_principal_atoms >= deployed_principal_atoms +
    ///    encumbered_in_orders_atoms`.
    /// This is what guarantees a depositor's withdrawable atoms are
    /// physically backed.
    pub async fn assert_vault_idle_invariant(&self, sub_vault_id: u16) {
        let p = self.read_sub_vault(sub_vault_id).await;
        let deployed_plus_encumbered: u128 =
            (p.deployed_principal_atoms as u128) + (p.encumbered_in_orders_atoms as u128);
        assert!(
            (p.total_principal_atoms as u128) >= deployed_plus_encumbered,
            "vault_idle invariant violated on profile {}: \
             total_principal({}) < deployed({}) + encumbered({}) = {}",
            sub_vault_id,
            p.total_principal_atoms,
            p.deployed_principal_atoms,
            p.encumbered_in_orders_atoms,
            deployed_plus_encumbered,
        );
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
