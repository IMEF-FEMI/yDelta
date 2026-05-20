//! Test fixtures for adapter integration tests. Boots a `ProgramTest`
//! environment loading three programs:
//!
//! - `marginfi.so` (mainnet v0.1.8 dump, in `tests/fixtures/`)
//! - `ydelta.so` (built from this workspace)
//! - `ydelta_test_harness.so` (the wrapper-ix program)
//!
//! Plus `add_account` helpers for synthesising marginfi `Bank` and
//! `MarginfiAccount` data directly into the test ledger so we can exercise
//! adapter read paths (shares↔amount conversions) without invoking the
//! real marginfi setup ixs (`marginfi_group_initialize`,
//! `lending_pool_add_bank`, etc.) — those land in a follow-up pass.

use std::cell::{RefCell, RefMut};
use std::path::PathBuf;
use std::rc::Rc;
use std::str::FromStr;

use base64::Engine;
use solana_program::pubkey::Pubkey;
use solana_program_test::{ProgramTest, ProgramTestContext};
use solana_sdk::account::Account;
use solana_sdk::instruction::Instruction;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::Transaction;

use marginfi_mocks::discriminator::{BANK_DISCRIMINATOR, MARGINFI_ACCOUNT_DISCRIMINATOR};
use marginfi_mocks::state::{
    Bank, MarginfiAccount, BANK_ACCOUNT_SIZE, MARGINFI_ACCOUNT_TOTAL_SIZE,
};
use marginfi_mocks::wire::WrappedI80F48;

/// Mainnet pubkeys we replay from `tests/fixtures/`.
pub mod mainnet {
    use solana_program::pubkey::Pubkey;
    pub fn marginfi_group() -> Pubkey {
        "4qp6Fx6tnZkY5Wropq9wUYgtFxXKwE6viZxFHg3rdAG8"
            .parse()
            .unwrap()
    }
    pub fn usdc_bank() -> Pubkey {
        "2s37akK2eyBbp8DZgCm7RtsaEz8eJP3Nxd4urLHQv7yB"
            .parse()
            .unwrap()
    }
    pub fn usdc_mint() -> Pubkey {
        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
            .parse()
            .unwrap()
    }
    pub fn usdc_liquidity_vault() -> Pubkey {
        "7jaiZR5Sk8hdYN9MxTpczTcwbWpb5WEoxSANuUwveuat"
            .parse()
            .unwrap()
    }
    pub fn usdc_oracle() -> Pubkey {
        "Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX"
            .parse()
            .unwrap()
    }
    pub fn sol_bank() -> Pubkey {
        "CCKtUs6Cgwo4aaQUmBPmyoApH2gUDErxNZCAntD6LYGh"
            .parse()
            .unwrap()
    }
    pub fn wsol_mint() -> Pubkey {
        "So11111111111111111111111111111111111111112"
            .parse()
            .unwrap()
    }
    pub fn sol_liquidity_vault() -> Pubkey {
        "2eicbpitfJXDwqCuFAmPgDP7t2oUotnAzbGzRKLMgSLe"
            .parse()
            .unwrap()
    }
    pub fn sol_oracle() -> Pubkey {
        "4Hmd6PdjVA9auCoScE12iaBogfwS4ZXQ6VZoBeqanwWW"
            .parse()
            .unwrap()
    }

    /// Derive `bank_liquidity_vault_authority` for a given bank. Marginfi
    /// uses `[b"liquidity_vault_auth", bank_pubkey]` as the seed.
    pub fn liquidity_vault_authority(bank: Pubkey) -> Pubkey {
        let (pda, _bump) = Pubkey::find_program_address(
            &[b"liquidity_vault_auth", bank.as_ref()],
            &marginfi_mocks::ID,
        );
        pda
    }
}

// AdapterFixture is consumed only by SBF-only tests; suppress the
// dead-code warning under the non-SBF build.
#[allow(dead_code)]
pub struct AdapterFixture {
    pub context: Rc<RefCell<ProgramTestContext>>,
    pub payer: Keypair,
}

#[allow(dead_code)]
impl AdapterFixture {
    /// Boot `ProgramTest` with marginfi + ydelta + ydelta-test-harness all
    /// loaded as SBPF binaries. Marginfi comes from the .so dumped from
    /// mainnet (`tests/fixtures/marginfi.so`); the other two are built
    /// from this workspace by `cargo test-sbf`.
    pub async fn new() -> Self {
        // `cargo test-sbf` sets SBF_OUT_DIR to `target/deploy/` (where it
        // builds ydelta.so + ydelta_test_harness.so). solana-program-test
        // looks up programs from that single dir. Copy marginfi.so (which
        // lives in tests/fixtures/) alongside the other .so files so all
        // three are discoverable from one place.
        Self::ensure_marginfi_so_in_sbf_dir();

        let mut program = ProgramTest::default();
        program.prefer_bpf(true);
        program.add_program("marginfi", marginfi_mocks::ID, None);
        program.add_program("ydelta", ydelta::ID, None);
        program.add_program("ydelta_test_harness", ydelta_test_harness::ID, None);

        let context = program.start_with_context().await;
        let payer = context.payer.insecure_clone();

        let fixture = Self {
            context: Rc::new(RefCell::new(context)),
            payer,
        };
        fixture
    }

    /// Boot a fresh `ProgramTest` and pre-load the mainnet marginfi state
    /// fixtures (group + USDC bank + USDC mint + liquidity vault) so
    /// integration tests can drive deposit/borrow/etc. flows against real
    /// marginfi state replayed from mainnet.
    pub async fn new_with_mainnet_marginfi() -> Self {
        Self::ensure_marginfi_so_in_sbf_dir();

        let mut program = ProgramTest::default();
        program.prefer_bpf(true);
        program.add_program("marginfi", marginfi_mocks::ID, None);
        program.add_program("ydelta", ydelta::ID, None);
        program.add_program("ydelta_test_harness", ydelta_test_harness::ID, None);

        // Load each mainnet fixture file as an account. The runtime will
        // expose them at the same pubkeys they had on mainnet.
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
        Self {
            context: Rc::new(RefCell::new(context)),
            payer,
        }
    }

    /// Process a single ix signed by the harness payer plus any extras.
    pub async fn process(
        &self,
        ix: Instruction,
        extra_signers: &[&Keypair],
    ) -> Result<(), solana_program_test::BanksClientError> {
        let payer = self.payer.insecure_clone();
        let blockhash = self.context.borrow().last_blockhash;
        let mut signers: Vec<&Keypair> = vec![&payer];
        signers.extend_from_slice(extra_signers);
        let tx =
            Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &signers, blockhash);
        let client: RefMut<ProgramTestContext> = self.context.borrow_mut();
        client.banks_client.process_transaction(tx).await
    }

    /// Refresh the cached blockhash (call between txs in the same test).
    pub async fn refresh_blockhash(&self) {
        let mut client: RefMut<ProgramTestContext> = self.context.borrow_mut();
        let bh = client.banks_client.get_latest_blockhash().await.unwrap();
        client.last_blockhash = bh;
    }

    /// Read an SPL token account's `amount` field (offset 64..72).
    pub async fn token_balance(&self, token_account: Pubkey) -> u64 {
        let client: RefMut<ProgramTestContext> = self.context.borrow_mut();
        let acc = client
            .banks_client
            .get_account(token_account)
            .await
            .unwrap()
            .expect("token account exists");
        u64::from_le_bytes(acc.data[64..72].try_into().unwrap())
    }

    /// Get raw account data (for inspecting marginfi-account state etc.).
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

    /// Refresh both oracle fixtures so marginfi's staleness checks pass.
    /// Reads the test ledger's `Clock` and mutates the dumped Pyth-pull
    /// (USDC) and Switchboard-pull (SOL) oracle accounts so their
    /// `publish_time` / `last_update_timestamp` / `result.slot` reflect the
    /// current ledger clock. Mirrors marginfi v2's `set_pyth_oracle_timestamp`
    /// helper (same pattern).
    ///
    /// Call this immediately before any health-checked CPI (borrow,
    /// withdraw). Repay/deposit don't run health checks so it's optional
    /// there.
    pub async fn refresh_oracle_freshness(&self) {
        let clock: solana_sdk::clock::Clock = {
            let ctx: RefMut<ProgramTestContext> = self.context.borrow_mut();
            ctx.banks_client.get_sysvar().await.unwrap()
        };
        self.refresh_pyth_pull_oracle(mainnet::usdc_oracle(), clock.unix_timestamp)
            .await;
        self.refresh_swb_pull_oracle(mainnet::sol_oracle(), clock.unix_timestamp, clock.slot)
            .await;
    }

    /// Mutate a Pyth-pull `PriceUpdateV2` account so its `publish_time` and
    /// `prev_publish_time` equal `timestamp`. The account body lives after
    /// the 8-byte Anchor discriminator; offsets are computed from the
    /// borsh-serialized `PriceUpdateV2` layout (write_authority(32) +
    /// verification_level(1) + price_message{feed_id(32) + price(8) +
    /// conf(8) + exponent(4) + publish_time(8) + prev_publish_time(8)
    /// + …}).
    pub async fn refresh_pyth_pull_oracle(&self, address: Pubkey, timestamp: i64) {
        const DISC: usize = 8;
        const PUBLISH_TIME_OFFSET: usize = DISC + 32 + 1 + 32 + 8 + 8 + 4; // = 93
        const PREV_PUBLISH_TIME_OFFSET: usize = PUBLISH_TIME_OFFSET + 8; // = 101

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

    /// Mutate a Switchboard-pull `PullFeedAccountData` account so its
    /// `last_update_timestamp` equals `timestamp` and `result.slot` equals
    /// `slot`. Offsets derived from the POD layout in
    /// `switchboard-on-demand::PullFeedAccountData`:
    /// - submissions: \[OracleSubmission(64); 32] = 2048 bytes
    /// - authority(32) + queue(32) + feed_hash(32) + initialized_at(8) +
    ///   permissions(8) + max_variance(8) + min_responses(4) + name(32) +
    ///   padding1(2) + historical_result_idx(1) + min_sample_size(1)
    ///   = 160 bytes → last_update_timestamp at struct offset 2208
    /// - + lut_slot(8) + _reserved1(32) → result starts at struct offset 2256
    ///   inside CurrentResult: value(16) + std_dev(16) + mean(16) + range(16)
    ///   + min_value(16) + max_value(16) + num_samples(1) + submission_idx(1)
    ///   + padding1(6) → slot at struct offset 2256 + 104 = 2360
    pub async fn refresh_swb_pull_oracle(&self, address: Pubkey, timestamp: i64, slot: u64) {
        const DISC: usize = 8;
        const LAST_UPDATE_TIMESTAMP_OFFSET: usize = DISC + 2208;
        const RESULT_SLOT_OFFSET: usize = DISC + 2360;

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

    /// Synthesise an SPL token account at `pubkey` for `mint` owned by
    /// `owner`, holding `amount` atoms. Bypasses the full SPL init flow
    /// since the state is what the deposit ix actually reads.
    pub fn put_token_account(&self, pubkey: Pubkey, mint: Pubkey, owner: Pubkey, amount: u64) {
        let mut data = vec![0u8; 165];
        data[0..32].copy_from_slice(mint.as_ref());
        data[32..64].copy_from_slice(owner.as_ref());
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        // `state` byte at offset 108: 1 = Initialized.
        data[108] = 1;
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

    /// Ensure `<SBF_OUT_DIR>/marginfi.so` exists so solana-program-test
    /// finds it. Resolves SBF_OUT_DIR (set by `cargo test-sbf`) and copies
    /// our fixtures-dir marginfi.so into it on first call. Idempotent.
    fn ensure_marginfi_so_in_sbf_dir() {
        let fixtures_marginfi =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/marginfi.so");
        let sbf_out = match std::env::var("SBF_OUT_DIR") {
            Ok(p) => PathBuf::from(p),
            Err(_) => {
                // No SBF_OUT_DIR: caller didn't run via `cargo test-sbf`.
                // Tests will fail to find ydelta.so; surface the real cause
                // up front.
                panic!("SBF_OUT_DIR not set. Run via `cargo test-sbf` or `scripts/test.sh --sbf`.");
            }
        };
        let target = sbf_out.join("marginfi.so");
        if !target.exists() {
            std::fs::copy(&fixtures_marginfi, &target)
                .unwrap_or_else(|e| panic!("failed to copy marginfi.so into {:?}: {}", sbf_out, e));
        }
    }

    /// Drop a synthetic marginfi `Bank` account into the test ledger at
    /// `bank_pubkey` with the given share-prices. No real marginfi setup
    /// flow runs — we just write the bytes the adapter needs to read.
    pub fn put_bank(
        &self,
        bank_pubkey: Pubkey,
        asset_share_value_bits: i128,
        liability_share_value_bits: i128,
    ) {
        // Build via Default + field assignment (struct-update `..Default::default()`
        // can't see private tail-padding fields from outside the marginfi-mocks crate).
        let mut bank: Bank = Default::default();
        bank.mint = Pubkey::new_unique();
        bank.mint_decimals = 6;
        bank.group = Pubkey::new_unique();
        bank.asset_share_value = WrappedI80F48::from_i128_bits(asset_share_value_bits);
        bank.liability_share_value = WrappedI80F48::from_i128_bits(liability_share_value_bits);
        bank.liquidity_vault = Pubkey::new_unique();
        bank.liquidity_vault_bump = 254;
        bank.liquidity_vault_authority_bump = 253;
        let mut data = Vec::with_capacity(BANK_ACCOUNT_SIZE);
        data.extend_from_slice(&BANK_DISCRIMINATOR);
        data.extend_from_slice(bytemuck::bytes_of(&bank));
        assert_eq!(data.len(), BANK_ACCOUNT_SIZE);

        let lamports = solana_sdk::rent::Rent::default().minimum_balance(BANK_ACCOUNT_SIZE);
        let acct = Account {
            lamports,
            data,
            owner: marginfi_mocks::ID,
            executable: false,
            rent_epoch: 0,
        };
        self.context
            .borrow_mut()
            .set_account(&bank_pubkey, &acct.into());
    }

    /// Drop a synthetic marginfi-account into the ledger with one active
    /// balance for `bank_pk` carrying the given share counts.
    #[allow(dead_code)]
    pub fn put_marginfi_account_with_balance(
        &self,
        marginfi_account_pubkey: Pubkey,
        authority: Pubkey,
        bank_pk: Pubkey,
        asset_shares_bits: i128,
        liability_shares_bits: i128,
    ) {
        let mut acc = MarginfiAccount::default();
        acc.group = Pubkey::new_unique();
        acc.authority = authority;
        let mut bal: marginfi_mocks::state::Balance = Default::default();
        bal.active = 1;
        bal.bank_pk = bank_pk;
        bal.asset_shares = WrappedI80F48::from_i128_bits(asset_shares_bits);
        bal.liability_shares = WrappedI80F48::from_i128_bits(liability_shares_bits);
        acc.balances[0] = bal;
        let mut data = Vec::with_capacity(MARGINFI_ACCOUNT_TOTAL_SIZE);
        data.extend_from_slice(&MARGINFI_ACCOUNT_DISCRIMINATOR);
        data.extend_from_slice(bytemuck::bytes_of(&acc));

        let lamports =
            solana_sdk::rent::Rent::default().minimum_balance(MARGINFI_ACCOUNT_TOTAL_SIZE);
        let acct = Account {
            lamports,
            data,
            owner: marginfi_mocks::ID,
            executable: false,
            rent_epoch: 0,
        };
        self.context
            .borrow_mut()
            .set_account(&marginfi_account_pubkey, &acct.into());
    }
}

/// Decode a `solana account ... --output json` dump file into an
/// `Account` ready for `ProgramTest::add_account`.
pub fn load_account_from_fixture(filename: &str) -> Account {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(filename);
    let raw = std::fs::read_to_string(&path).expect("fixture file exists");
    let json: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    let acc = json.get("account").unwrap_or(&json);
    let data_field = &acc["data"];
    let data = if let Some(arr) = data_field.as_array() {
        // Standard `solana account --output json` shape: ["base64", "base64"].
        let b64 = arr[0].as_str().expect("base64 string at data[0]");
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("decode base64")
    } else {
        Vec::new()
    };
    let owner = Pubkey::from_str(acc["owner"].as_str().unwrap()).unwrap();
    let lamports = acc["lamports"].as_u64().unwrap();
    let executable = acc["executable"].as_bool().unwrap_or(false);
    let rent_epoch = acc["rentEpoch"].as_u64().unwrap_or(0);
    Account {
        lamports,
        data,
        owner,
        executable,
        rent_epoch,
    }
}
