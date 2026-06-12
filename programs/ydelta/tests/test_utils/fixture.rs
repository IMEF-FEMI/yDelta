use std::cell::{RefCell, RefMut};
use std::rc::Rc;

use hypertree::{
    DataIndex, HyperTreeReadOperations, HyperTreeValueIteratorTrait, RedBlackTreeReadOnly, NIL,
};
use solana_program::pubkey::Pubkey;
#[cfg(not(feature = "test-sbf"))]
use solana_program_test::processor;
use solana_program_test::{ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::Account,
    program_pack::Pack,
    signature::{Keypair, Signer},
    system_instruction,
    transaction::Transaction,
};
use spl_token::state::{Account as TokenAccount, Mint};

use ydelta::program::instruction_builders::{
    claim_seat_instruction::claim_seat_instruction,
    create_market_instructions::create_market_instructions,
    place_order_instruction::place_order_instruction,
};
use ydelta::program::processor::create_market::CreateMarketParams;
use ydelta::state::market::get_mut_helper_seat;
use ydelta::state::{ClaimedSeat, MarketFixed, MarketValue, OrderType, RestingOrder, Side};

use marginfi_mocks::discriminator::BANK_DISCRIMINATOR;
use marginfi_mocks::state::{Bank, BANK_ACCOUNT_SIZE};

use super::marginfi_fixture::{load_account_from_fixture, mainnet};

pub const DEBT_DECIMALS: u8 = 6;
pub const COLLATERAL_DECIMALS: u8 = 9;

/// Test fixture covering one market with two SPL mints. Signers (the payer
/// and any extra `Keypair`s minted via `mint_keypair`) come pre-funded with
/// SOL for rent and with both mints' ATAs created and topped up.
///
/// Loads `marginfi.so` and the real mainnet `marginfi_group` account
/// so `process_create_market` can CPI into
/// `marginfi_account_initialize`. The marginfi-account itself is a
/// per-market PDA created by that CPI — no separate `marginfi_account`
/// field is needed. Banks remain synthetic stubs (correct
/// discriminator + ownership) since `create_market` validates them
/// but does not pass them to marginfi.
pub struct TestFixture {
    pub context: Rc<RefCell<ProgramTestContext>>,
    pub debt_mint: Keypair,
    pub collateral_mint: Keypair,
    pub market: Keypair,
    pub marginfi_group: Pubkey,
    pub debt_bank: Pubkey,
    pub collateral_bank: Pubkey,
    /// Synthesised oracle pubkey written into each synthetic bank's
    /// `config.oracle_keys[0]`. Real oracle decoding never runs against
    /// these accounts in TestFixture — the matching engine validates
    /// the address only, not the body. The full LTV-at-match path
    /// (which uses actual prices) requires real oracle accounts and
    /// lives under MarketFixture instead.
    pub debt_oracle: Pubkey,
    pub collateral_oracle: Pubkey,
    /// Synthesised debt-side liquidity vault. Validated by
    /// `PlaceOrderContext` via `debt_bank.liquidity_vault`.
    pub debt_liquidity_vault: Pubkey,
    pub collateral_liquidity_vault: Pubkey,
    /// Per-signer debt-mint ATA cache. `place_order` synth-creates
    /// one per signer on first call so the loader's owner/mint
    /// validation on `borrower_debt_token` passes even for non-P2Pool
    /// tests.
    signer_debt_tokens: RefCell<std::collections::HashMap<Pubkey, Pubkey>>,
}

#[allow(dead_code)] // test fixture; some helpers are reserved for future cases
impl TestFixture {
    pub async fn new() -> Self {
        // With `test-sbf` enabled, ProgramTest loads `ydelta.so` via SBF_OUT_DIR
        // (set by `cargo test-sbf`) and runs the program through the real SBPF
        // VM. Without the feature, the program is linked natively via
        // `processor!` for fast iteration.
        #[cfg(not(feature = "test-sbf"))]
        let mut program: ProgramTest = ProgramTest::new(
            "ydelta",
            ydelta::ID,
            processor!(ydelta::process_instruction),
        );
        #[cfg(feature = "test-sbf")]
        let mut program: ProgramTest = ProgramTest::new("ydelta", ydelta::ID, None);

        // Marginfi must be loaded for `process_create_market`'s
        // `marginfi_account_initialize` CPI. `add_program(_, _, None)` only
        // routes to the BPF loader when `prefer_bpf` is true on the
        // `ProgramTest`. Native mode initialises with `prefer_bpf=false`
        // (since ydelta is a builtin), so we flip the flag for the marginfi
        // call and restore it afterwards. Under `test-sbf` the flag is
        // already true (BPF_OUT_DIR is set) and the toggles are no-ops.
        let prev_prefer_bpf =
            std::env::var_os("BPF_OUT_DIR").is_some() || std::env::var_os("SBF_OUT_DIR").is_some();
        program.prefer_bpf(true);
        program.add_program("marginfi", marginfi_mocks::ID, None);
        program.prefer_bpf(prev_prefer_bpf);

        // Real mainnet marginfi_group fixture — marginfi deserialises the
        // group during `marginfi_account_initialize` so a synth stub with
        // a zero-body would fault inside marginfi.
        let marginfi_group = mainnet::marginfi_group();
        program.add_account(
            marginfi_group,
            load_account_from_fixture("marginfi_group.json"),
        );

        let debt_bank = Pubkey::new_unique();
        let collateral_bank = Pubkey::new_unique();
        let debt_oracle = Pubkey::new_unique();
        let collateral_oracle = Pubkey::new_unique();
        let debt_liquidity_vault = Pubkey::new_unique();
        let collateral_liquidity_vault = Pubkey::new_unique();

        let context = Rc::new(RefCell::new(program.start_with_context().await));

        let debt_mint = Keypair::new();
        let collateral_mint = Keypair::new();
        let market = Keypair::new();

        let mut fixture = TestFixture {
            context,
            debt_mint,
            collateral_mint,
            market,
            marginfi_group,
            debt_bank,
            collateral_bank,
            debt_oracle,
            collateral_oracle,
            debt_liquidity_vault,
            collateral_liquidity_vault,
            signer_debt_tokens: RefCell::new(std::collections::HashMap::new()),
        };

        fixture
            .create_mint_with_decimals(&fixture.debt_mint.insecure_clone(), DEBT_DECIMALS)
            .await;
        fixture
            .create_mint_with_decimals(
                &fixture.collateral_mint.insecure_clone(),
                COLLATERAL_DECIMALS,
            )
            .await;
        fixture.synth_marginfi_bank_stubs();
        fixture.synth_liquidity_vaults();
        fixture.synth_pyth_oracles();
        fixture.create_global_config_account().await;
        fixture.create_market_account().await;
        fixture
    }

    /// Every state-mutating ix requires the singleton `GlobalConfig`
    /// PDA. Stand it up once at fixture setup; the fixture's payer
    /// becomes `protocol_admin`.
    async fn create_global_config_account(&self) {
        let payer = self.payer_keypair();
        // Synthesize a fake `BpfLoaderUpgradeable::ProgramData`
        // account at the expected PDA so the loader's
        // payer == upgrade_authority gate passes under ProgramTest
        // (which loads the .so without going through
        // BpfLoaderUpgradeable).
        synth_program_data_account(&self.context, &payer.pubkey());
        let ix = ydelta::program::instruction_builders::global_config_admin_instructions::create_global_config_instruction(
            &payer.pubkey(),
        );
        let blockhash = self.context.borrow().last_blockhash;
        let tx = solana_sdk::transaction::Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            blockhash,
        );
        self.context
            .borrow_mut()
            .banks_client
            .process_transaction(tx)
            .await
            .unwrap();
    }

    /// Drop stub marginfi `Bank` accounts at `debt_bank` and
    /// `collateral_bank` into the test ledger. Each carries the correct
    /// Anchor discriminator and is owned by `marginfi_mocks::ID`, so
    /// `process_create_market`'s loader validation passes. These are NOT
    /// real marginfi state — banks aren't passed to the
    /// `marginfi_account_initialize` CPI, so stubs are fine here. Adapter
    /// tests that exercise deposit/withdraw/borrow CPIs use the real
    /// mainnet bank fixtures via `AdapterFixture`.
    fn synth_marginfi_bank_stubs(&self) {
        // Body-relative offsets within the BankConfig region, copied
        // from `marginfi_mocks::state::BankConfigView`'s constants. Used
        // to poke synthetic oracle / weight bytes into the otherwise
        // opaque tail so `PlaceOrderContext`'s validation accepts the
        // stub banks. Real banks have these populated through
        // marginfi's own initialisation; here we mimic the shape.
        const BANK_CONFIG_BODY_OFFSET: usize = 288;
        const BC_ASSET_WEIGHT_INIT: usize = 0;
        const BC_LIABILITY_WEIGHT_INIT: usize = 32;
        const BC_ORACLE_SETUP: usize = 313;
        const BC_ORACLE_KEYS: usize = 314;
        const BC_ORACLE_MAX_AGE: usize = 504;
        const ANCHOR_DISC_LEN: usize = 8;

        for (bank_pk, mint_pk, oracle_pk, lv_pk) in [
            (
                self.debt_bank,
                self.debt_mint.pubkey(),
                self.debt_oracle,
                self.debt_liquidity_vault,
            ),
            (
                self.collateral_bank,
                self.collateral_mint.pubkey(),
                self.collateral_oracle,
                self.collateral_liquidity_vault,
            ),
        ] {
            let mut bank: Bank = Default::default();
            bank.mint = mint_pk;
            bank.mint_decimals = 6;
            bank.group = self.marginfi_group;
            bank.asset_share_value =
                marginfi_mocks::wire::WrappedI80F48::from_i128_bits(1i128 << 48);
            bank.liability_share_value =
                marginfi_mocks::wire::WrappedI80F48::from_i128_bits(1i128 << 48);
            bank.liquidity_vault = lv_pk;
            let mut data = Vec::with_capacity(BANK_ACCOUNT_SIZE);
            data.extend_from_slice(&BANK_DISCRIMINATOR);
            data.extend_from_slice(bytemuck::bytes_of(&bank));

            // Poke BankConfig fields directly into the opaque tail at
            // the documented offsets. `data` already has the 8-byte
            // disc, so absolute offsets are ANCHOR_DISC_LEN +
            // BANK_CONFIG_BODY_OFFSET + field.
            let cfg = ANCHOR_DISC_LEN + BANK_CONFIG_BODY_OFFSET;
            // Neutral weights (1.0 / 1.0) so existing matching tests
            // — which use simple `principal == collateral × ratio`
            // collateral picks — pass the LTV check at the synthetic
            // $1 oracle prices. Realistic-weight LTV behaviour is
            // exercised by `MarketFixture`-backed SBF tests.
            let one_fp48: i128 = 1i128 << 48;
            data[cfg + BC_ASSET_WEIGHT_INIT..cfg + BC_ASSET_WEIGHT_INIT + 16]
                .copy_from_slice(&one_fp48.to_le_bytes());
            data[cfg + BC_LIABILITY_WEIGHT_INIT..cfg + BC_LIABILITY_WEIGHT_INIT + 16]
                .copy_from_slice(&one_fp48.to_le_bytes());
            // oracle_setup = PythPushOracle (3); never decoded against
            // these stub oracle accounts — PlaceOrderContext only
            // checks the pubkey matches. Step 8's match-time price
            // read uses MarketFixture's real banks.
            data[cfg + BC_ORACLE_SETUP] = 3;
            data[cfg + BC_ORACLE_KEYS..cfg + BC_ORACLE_KEYS + 32]
                .copy_from_slice(oracle_pk.as_ref());
            // oracle_max_age = 600 (long enough that the staleness
            // check is moot for these stubs).
            data[cfg + BC_ORACLE_MAX_AGE..cfg + BC_ORACLE_MAX_AGE + 2]
                .copy_from_slice(&600u16.to_le_bytes());

            self.set_account_owned_by_marginfi(bank_pk, data);
        }
    }

    /// Plant a Pyth-push (`PriceUpdateV2`) account at each synthetic
    /// oracle pubkey so the matching engine's `oracle_price` decode
    /// succeeds. Sets price = 1_000_000 atoms with exponent = -6 →
    /// $1.00 fp48. publish_time = host wall-clock so `Clock - publish ≈
    /// 0` and the staleness gate accepts.
    ///
    /// **NOT real Pyth state** — the bytes only need to satisfy the
    /// adapter's offset reads. Tests asserting real price values
    /// (Step 8 LTV tests) should use `MarketFixture` with real
    /// mainnet oracle dumps.
    fn synth_pyth_oracles(&self) {
        // Account-data offsets within `PriceUpdateV2` (post-8-byte
        // disc). Mirrored from `protocol/oracles.rs` constants.
        const PYTH_PRICE_OFFSET: usize = 73;
        const PYTH_CONF_OFFSET: usize = 81;
        const PYTH_EXPONENT_OFFSET: usize = 89;
        const PYTH_PUBLISH_TIME_OFFSET: usize = 93;
        const PYTH_BODY_SIZE: usize = 200;

        // Price = 1_000_000, exponent = -6 → $1.00 USD per token.
        // Both debt and collateral oracles share this so collateral
        // and debt are equally priced; the tests don't require
        // distinct prices.
        let price: i64 = 1_000_000;
        let conf: u64 = 100;
        let exponent: i32 = -6;
        // Solana's ProgramTest seeds Clock.unix_timestamp from the host's
        // real time at boot. Match it here so the staleness gate sees
        // age ≈ 0. Tests that advance the clock past the bank's 600s
        // max_age call set_clock_unix and refresh oracles explicitly.
        let publish_time: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        for oracle_pk in [self.debt_oracle, self.collateral_oracle] {
            // Use a recognisable but inert disc — not validated by
            // ydelta's oracle reader; only marginfi's own
            // PriceUpdateV2 disc check would matter, and that's not
            // run by us.
            let mut data = vec![0u8; PYTH_BODY_SIZE];
            // verification_level byte (post-disc + write_authority).
            // Offset 40 = 8 (disc) + 32 (write_authority). Set to 0x01
            // (Full) — ydelta's oracle reader enforces a Full minimum
            // (mirroring marginfi's `MIN_PYTH_PUSH_VERIFICATION_LEVEL`).
            data[40] = 1;
            data[PYTH_PRICE_OFFSET..PYTH_PRICE_OFFSET + 8].copy_from_slice(&price.to_le_bytes());
            data[PYTH_CONF_OFFSET..PYTH_CONF_OFFSET + 8].copy_from_slice(&conf.to_le_bytes());
            data[PYTH_EXPONENT_OFFSET..PYTH_EXPONENT_OFFSET + 4]
                .copy_from_slice(&exponent.to_le_bytes());
            data[PYTH_PUBLISH_TIME_OFFSET..PYTH_PUBLISH_TIME_OFFSET + 8]
                .copy_from_slice(&publish_time.to_le_bytes());

            let lamports = solana_sdk::rent::Rent::default().minimum_balance(data.len());
            // Owner must match what yDelta's adapter expects (Pyth
            // Solana Receiver Program). Synthetic fixture mirrors
            // the production owner for the adapter's owner check.
            let acc = Account {
                lamports,
                data,
                owner: ydelta::protocol::oracles::PYTH_PUSH_PROGRAM_ID,
                executable: false,
                rent_epoch: 0,
            };
            self.context
                .borrow_mut()
                .set_account(&oracle_pk, &acc.into());
        }
    }

    /// Plant an SPL token account at each synthetic liquidity_vault
    /// pubkey so `PlaceOrderContext`'s `TokenAccountInfo::new` (which
    /// requires SPL-token-program owner + matching mint) succeeds.
    /// Balance starts at zero — these vaults are never touched at
    /// match time in the matching/encumbrance/self-match tests
    /// (Step 8's P2Pool fallback that would actually mutate them is
    /// gated on `OB_ONLY` semantics that the affected tests don't
    /// exercise).
    fn synth_liquidity_vaults(&self) {
        for (vault_pk, mint_pk) in [
            (self.debt_liquidity_vault, self.debt_mint.pubkey()),
            (
                self.collateral_liquidity_vault,
                self.collateral_mint.pubkey(),
            ),
        ] {
            let mut data = vec![0u8; 165];
            data[0..32].copy_from_slice(mint_pk.as_ref());
            // owner field at 32..64 — leave at default; the loader
            // doesn't check vault.owner because marginfi liquidity
            // vaults are owned by `bank_liquidity_vault_authority`
            // (a marginfi PDA), not by the token-account holder.
            // amount at 64..72 = 0 (zeroed by vec! init).
            data[108] = 1; // state = Initialized
            let lamports = solana_sdk::rent::Rent::default().minimum_balance(165);
            let acc = Account {
                lamports,
                data,
                owner: spl_token::id(),
                executable: false,
                rent_epoch: 0,
            };
            self.context
                .borrow_mut()
                .set_account(&vault_pk, &acc.into());
        }
    }

    fn set_account_owned_by_marginfi(&self, pubkey: Pubkey, data: Vec<u8>) {
        let lamports = solana_sdk::rent::Rent::default().minimum_balance(data.len());
        let acc = Account {
            lamports,
            data,
            owner: marginfi_mocks::ID,
            executable: false,
            rent_epoch: 0,
        };
        self.context.borrow_mut().set_account(&pubkey, &acc.into());
    }

    fn last_blockhash(&self) -> solana_program::hash::Hash {
        self.context.borrow().last_blockhash
    }

    fn payer_pubkey(&self) -> Pubkey {
        self.context.borrow().payer.pubkey()
    }

    pub fn payer_keypair(&self) -> Keypair {
        self.context.borrow().payer.insecure_clone()
    }

    pub fn debt_mint_key(&self) -> Pubkey {
        self.debt_mint.pubkey()
    }
    pub fn collateral_mint_key(&self) -> Pubkey {
        self.collateral_mint.pubkey()
    }
    pub fn market_key(&self) -> Pubkey {
        self.market.pubkey()
    }

    async fn process(
        &self,
        ixs: &[solana_program::instruction::Instruction],
        signers: &[&Keypair],
    ) -> Result<(), solana_program_test::BanksClientError> {
        let blockhash = self.last_blockhash();
        let payer = self.payer_keypair();
        let mut all: Vec<&Keypair> = vec![&payer];
        for s in signers {
            all.push(*s);
        }
        let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, blockhash);
        let client: RefMut<ProgramTestContext> = self.context.borrow_mut();
        client.banks_client.process_transaction(tx).await
    }

    async fn create_mint_with_decimals(&mut self, mint_kp: &Keypair, decimals: u8) {
        let payer = self.payer_keypair();
        let rent_lamports = solana_program::rent::Rent::default().minimum_balance(Mint::LEN);

        let create_acct = system_instruction::create_account(
            &payer.pubkey(),
            &mint_kp.pubkey(),
            rent_lamports,
            Mint::LEN as u64,
            &spl_token::id(),
        );
        let init_mint = spl_token::instruction::initialize_mint2(
            &spl_token::id(),
            &mint_kp.pubkey(),
            &payer.pubkey(),
            None,
            decimals,
        )
        .unwrap();
        self.process(&[create_acct, init_mint], &[mint_kp])
            .await
            .unwrap();
    }

    async fn create_market_account(&mut self) {
        let payer = self.payer_keypair();
        // Override `ltv_buffer_bps` back to 0 — the protocol default
        // moved to 200 (2%) once `create_market` started seeding a safe
        // `FeeConfig::default()`, but existing tests in this suite
        // were authored against a zero buffer (their LTV math is
        // calibrated against the bare oracle minimum). Holding the
        // buffer at 0 here avoids re-tuning every LTV-sensitive case.
        let params = CreateMarketParams {
            ltv_buffer_bps: Some(0),
            ..CreateMarketParams::default()
        };
        let ixs = create_market_instructions(
            &self.market.pubkey(),
            &self.debt_mint.pubkey(),
            &self.collateral_mint.pubkey(),
            &payer.pubkey(),
            &self.marginfi_group,
            &self.debt_bank,
            &self.collateral_bank,
            &marginfi_mocks::ID,
            &params,
        )
        .unwrap();
        let market_kp = self.market.insecure_clone();
        self.process(&ixs, &[&market_kp]).await.unwrap();
    }

    /// Create a fresh keypair pre-funded with SOL and ATAs for both mints.
    /// Mints `debt_balance` debt-atoms and `collateral_balance` collateral
    /// atoms into the new account's ATAs.
    pub async fn create_trader(&mut self, debt_balance: u64, collateral_balance: u64) -> Keypair {
        let trader = Keypair::new();
        let payer = self.payer_keypair();

        // Fund with SOL for rent.
        let fund = system_instruction::transfer(&payer.pubkey(), &trader.pubkey(), 10_000_000_000);
        self.process(&[fund], &[]).await.unwrap();

        for (mint, balance) in [
            (self.debt_mint.pubkey(), debt_balance),
            (self.collateral_mint.pubkey(), collateral_balance),
        ] {
            let ata = self.create_token_account(&trader.pubkey(), &mint).await;
            if balance > 0 {
                let mint_to = spl_token::instruction::mint_to(
                    &spl_token::id(),
                    &mint,
                    &ata,
                    &payer.pubkey(),
                    &[],
                    balance,
                )
                .unwrap();
                self.process(&[mint_to], &[]).await.unwrap();
            }
        }

        trader
    }

    async fn create_token_account(&self, owner: &Pubkey, mint: &Pubkey) -> Pubkey {
        let payer = self.payer_keypair();
        let ata_kp = Keypair::new();
        let rent_lamports =
            solana_program::rent::Rent::default().minimum_balance(TokenAccount::LEN);

        let create = system_instruction::create_account(
            &payer.pubkey(),
            &ata_kp.pubkey(),
            rent_lamports,
            TokenAccount::LEN as u64,
            &spl_token::id(),
        );
        let init = spl_token::instruction::initialize_account3(
            &spl_token::id(),
            &ata_kp.pubkey(),
            mint,
            owner,
        )
        .unwrap();
        self.process(&[create, init], &[&ata_kp]).await.unwrap();
        ata_kp.pubkey()
    }

    pub async fn claim_seat(
        &self,
        signer: &Keypair,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = claim_seat_instruction(&self.market.pubkey(), &signer.pubkey());
        let kp = signer.insecure_clone();
        self.process(&[ix], &[&kp]).await
    }

    // Deposit/withdraw require real marginfi bank state (CPI into
    // marginfi.so). The synthetic banks `TestFixture` carries are good
    // enough for `create_market` (which doesn't touch banks during
    // `marginfi_account_initialize`) but not for
    // `lending_account_deposit`. Tests that just need seat-share state
    // to drive matching/ordering go through `seed_seat_shares` below
    // — a backdoor that writes shares directly without touching SPL
    // or marginfi.

    /// Test backdoor: bump a seat's `*_withdrawable_shares` directly,
    /// bypassing the deposit ix. Used by matching/ordering tests that
    /// just need the seat to carry funds for encumbrance — they don't
    /// care about the SPL or marginfi side. Reads the market account,
    /// mutates the seat in place using the same RB-tree helper the
    /// program does, and writes the bytes back via `set_account`.
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
            // Scale to I80F48 to match what marginfi actually credits on
            // deposit — the seat-share fields and the program's
            // `atoms_to_shares_at_snapshot` both work in this unit.
            // Callers pass `shares` as a whole-share count; we shift here
            // so existing tests don't need to thread the 2^48 scale.
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

    /// Place a borrower IOC bid.
    ///
    /// `place_order` is borrower-IOC-only. The `side` / `order_type`
    /// arguments are accepted for source compatibility but ignored —
    /// every call is a `Side::Bid` IOC.
    #[allow(clippy::too_many_arguments)]
    pub async fn place_order(
        &self,
        signer: &Keypair,
        _side: Side,
        _order_type: OrderType,
        rate_bps: u16,
        term_seconds: u32,
        principal_atoms: u64,
        collateral_atoms: u64,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let debt_bank_lva = self.derive_marginfi_lva(self.debt_bank);
        let borrower_debt_token = self.signer_debt_token(&signer.pubkey()).await;
        // Native TestFixture uses synthetic banks whose
        // `bank_liquidity_vault_authority` PDAs aren't real, so a real
        // `marginfi.borrow` CPI would fail with `ConstraintSeeds`. Set
        // `OB_ONLY` so the residual goes to `Drop` instead of
        // triggering the P2Pool fallback. Tests that explicitly want
        // P2Pool live under `MarketFixture`.
        let flags = ydelta::state::market_helpers::FLAG_OB_ONLY;
        let ix = place_order_instruction(
            &self.market.pubkey(),
            &signer.pubkey(),
            &self.marginfi_group,
            &self.debt_bank,
            &self.collateral_bank,
            &[self.debt_oracle],
            &[self.collateral_oracle],
            &self.debt_liquidity_vault,
            &debt_bank_lva,
            &borrower_debt_token,
            &self.debt_mint.pubkey(),
            &spl_token::id(),
            &marginfi_mocks::ID,
            rate_bps,
            term_seconds,
            principal_atoms,
            collateral_atoms,
            flags,
            None,
        );
        let kp = signer.insecure_clone();
        self.process(&[ix], &[&kp]).await
    }

    /// Derive marginfi's `bank_liquidity_vault_authority` PDA at
    /// `[b"liquidity_vault_auth", bank_pk]`.
    pub fn derive_marginfi_lva(&self, bank_pk: Pubkey) -> Pubkey {
        Pubkey::find_program_address(
            &[b"liquidity_vault_auth", bank_pk.as_ref()],
            &marginfi_mocks::ID,
        )
        .0
    }

    pub async fn read_market(&self) -> MarketValue {
        let client: RefMut<ProgramTestContext> = self.context.borrow_mut();
        let acct: Account = client
            .banks_client
            .get_account(self.market.pubkey())
            .await
            .unwrap()
            .expect("market exists");
        let bytes = acct.data;
        let (header, dynamic) = bytes.split_at(std::mem::size_of::<MarketFixed>());
        let fixed: MarketFixed = *bytemuck::from_bytes::<MarketFixed>(header);
        MarketValue {
            fixed,
            dynamic: dynamic.to_vec(),
        }
    }

    /// Walk the market's claimed_seats tree, returning the seat for `owner`
    /// (with `sub_vault_id = 0`). Panics if not present.
    pub async fn read_seat(&self, owner: &Pubkey) -> ClaimedSeat {
        let market = self.read_market().await;
        let tree: RedBlackTreeReadOnly<ClaimedSeat> =
            RedBlackTreeReadOnly::new(&market.dynamic, market.fixed.claimed_seats_root_index, NIL);
        let probe = ClaimedSeat::new_empty(*owner, 0, 0);
        let idx: DataIndex = tree.lookup_index(&probe);
        assert_ne!(idx, NIL, "no seat for {}", owner);
        let node = ydelta::state::market::get_helper_seat(&market.dynamic, idx);
        *node.get_value()
    }

    /// Count the resting orders on a given side of the market. Only
    /// the asks tree holds resting orders — a borrower bid never rests,
    /// so `Side::Bid` is always 0.
    pub async fn count_orders(&self, side: Side) -> usize {
        if side == Side::Bid {
            return 0;
        }
        let market = self.read_market().await;
        let tree: RedBlackTreeReadOnly<RestingOrder> = RedBlackTreeReadOnly::new(
            &market.dynamic,
            market.fixed.asks_root_index,
            market.fixed.asks_best_index,
        );
        tree.iter::<RestingOrder>().count()
    }

    /// Last blockhash refresh — call when your test needs to send another
    /// tx without colliding on dedupe.
    pub async fn refresh_blockhash(&self) {
        let mut client: RefMut<ProgramTestContext> = self.context.borrow_mut();
        let new_hash = client.banks_client.get_latest_blockhash().await.unwrap();
        client.last_blockhash = new_hash;
    }

    pub fn banks_client(&self) -> Rc<RefCell<ProgramTestContext>> {
        Rc::clone(&self.context)
    }

    /// Return the debt-mint token-account address for `signer`, creating
    /// it on first call. Used by `place_order` to pass a valid
    /// `borrower_debt_token` account through the loader's owner/mint
    /// validation; P2Pool-borrowed atoms land here when the residual
    /// triggers `marginfi.borrow`.
    pub async fn signer_debt_token(&self, signer: &Pubkey) -> Pubkey {
        if let Some(addr) = self.signer_debt_tokens.borrow().get(signer) {
            return *addr;
        }
        let debt_mint = self.debt_mint.pubkey();
        let ata = self.create_token_account(signer, &debt_mint).await;
        self.signer_debt_tokens.borrow_mut().insert(*signer, ata);
        ata
    }
}

impl TestFixture {
    /// Create + initialise an SPL token account for `owner`, optionally
    /// minting `initial_atoms` into it.
    pub async fn create_token_account_and_mint(
        &self,
        owner: &Pubkey,
        mint: &Pubkey,
        initial_atoms: u64,
    ) -> Pubkey {
        let ata = self.create_token_account(owner, mint).await;
        if initial_atoms > 0 {
            let payer = self.payer_keypair();
            let mint_to = spl_token::instruction::mint_to(
                &spl_token::id(),
                mint,
                &ata,
                &payer.pubkey(),
                &[],
                initial_atoms,
            )
            .unwrap();
            self.process(&[mint_to], &[]).await.unwrap();
        }
        ata
    }
}

/// Test helper: synthesize a `BpfLoaderUpgradeable::ProgramData`
/// account at `[ydelta::ID]` under `bpf_loader_upgradeable::id()`,
/// with `upgrade_authority` set to `expected_authority`. Layout (45
/// bytes; bincode-serialized `UpgradeableLoaderState::ProgramData`):
///   bytes 0..4   : enum tag (3 = ProgramData), u32 LE
///   bytes 4..12  : slot, u64 LE (zero in tests)
///   byte  12     : Option<Pubkey> tag (1 = Some)
///   bytes 13..45 : upgrade_authority pubkey
pub fn synth_program_data_account(
    context: &Rc<RefCell<ProgramTestContext>>,
    expected_authority: &Pubkey,
) {
    let (program_data_addr, _) = Pubkey::find_program_address(
        &[ydelta::ID.as_ref()],
        &solana_program::bpf_loader_upgradeable::id(),
    );
    let mut data = vec![0u8; 45];
    data[0..4].copy_from_slice(&3u32.to_le_bytes());
    data[12] = 1; // Some(upgrade_authority)
    data[13..45].copy_from_slice(expected_authority.as_ref());
    let account = Account {
        lamports: 1,
        data,
        owner: solana_program::bpf_loader_upgradeable::id(),
        executable: false,
        rent_epoch: 0,
    };
    context
        .borrow_mut()
        .set_account(&program_data_addr, &account.into());
}
