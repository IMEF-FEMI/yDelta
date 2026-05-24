//! Integration test for the read-only adapter conversions
//! (`shares_to_amount`, `amount_to_shares`). Loads marginfi.so + ydelta.so
//! + ydelta-test-harness.so, drops a synthetic Bank into the ledger, then
//! invokes the harness ix which CPIs into ydelta's adapter logic.
//!
//! Verifies:
//! 1. Programs load without crash → confirms our SBPF/load-order setup.
//! 2. The adapter reads the synthetic Bank's `asset_share_value` correctly.
//! 3. Conversion math at unit (1.0) and non-unit (1.5) share-prices matches
//!    expectations.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::instruction::{AccountMeta, Instruction};
use solana_program_test::ProgramTestContext;
use solana_sdk::{pubkey::Pubkey, signer::Signer, transaction::Transaction};

use ydelta_test_harness::{AmountToSharesParams, HarnessInstruction, SharesToAmountParams};

use crate::test_utils::AdapterFixture;

const HARNESS_TAG_SHARES_TO_AMOUNT: u8 = HarnessInstruction::SharesToAmount as u8;
const HARNESS_TAG_AMOUNT_TO_SHARES: u8 = HarnessInstruction::AmountToShares as u8;

fn shares_to_amount_ix(bank: Pubkey, shares: u128) -> Instruction {
    let mut data = vec![HARNESS_TAG_SHARES_TO_AMOUNT];
    SharesToAmountParams { shares }
        .serialize(&mut data)
        .unwrap();
    Instruction {
        program_id: ydelta_test_harness::ID,
        accounts: vec![AccountMeta::new_readonly(bank, false)],
        data,
    }
}

fn amount_to_shares_ix(bank: Pubkey, amount: u64) -> Instruction {
    let mut data = vec![HARNESS_TAG_AMOUNT_TO_SHARES];
    AmountToSharesParams { amount }
        .serialize(&mut data)
        .unwrap();
    Instruction {
        program_id: ydelta_test_harness::ID,
        accounts: vec![AccountMeta::new_readonly(bank, false)],
        data,
    }
}

/// Decoded `AdapterResult` payload from a `sol_log_data` event.
#[derive(BorshDeserialize)]
struct AdapterResult {
    which: u8,
    atoms_or_shares: u128,
}

/// Run a transaction containing the given ix and return the decoded
/// adapter result from the program logs.
async fn run_and_read_result(fixture: &AdapterFixture, ix: Instruction) -> AdapterResult {
    let payer = fixture.payer.insecure_clone();
    let blockhash = fixture.context.borrow().last_blockhash;
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], blockhash);

    let logs = {
        let client: RefMut<ProgramTestContext> = fixture.context.borrow_mut();
        let sim = client
            .banks_client
            .simulate_transaction(tx.clone())
            .await
            .unwrap();
        sim.simulation_details
            .expect("simulation produced details")
            .logs
    };

    // Anchor-style sol_log_data lines look like: "Program data: <base64>".
    for line in logs.iter().rev() {
        if let Some(rest) = line.strip_prefix("Program data: ") {
            // Use solana's base64 — pull via solana_sdk's bs58/base64? Simpler:
            // sol_log_data uses base64::STANDARD; decode with the `base64` crate
            // if available. Here we use solana_program's logging which writes
            // standard base64 — decode with std-only via a tiny helper.
            if let Ok(bytes) = base64_decode(rest.trim()) {
                if let Ok(r) = AdapterResult::try_from_slice(&bytes) {
                    return r;
                }
            }
        }
    }
    panic!("no AdapterResult found in tx logs:\n{:#?}", logs);
}

/// Minimal base64 decoder (RFC 4648 standard alphabet) so the test crate
/// doesn't need to pull in the `base64` dep just for one line.
fn base64_decode(s: &str) -> Result<Vec<u8>, ()> {
    fn val(c: u8) -> Result<u8, ()> {
        match c {
            b'A'..=b'Z' => Ok(c - b'A'),
            b'a'..=b'z' => Ok(c - b'a' + 26),
            b'0'..=b'9' => Ok(c - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(()),
        }
    }
    let s = s.trim_end_matches('=');
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4 + 1);
    let mut buf: u32 = 0;
    let mut bits = 0;
    for &c in bytes {
        let v = val(c)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Ok(out)
}

#[tokio::test]
async fn shares_to_amount_at_unit_price() {
    let fixture = AdapterFixture::new().await;
    let bank = Pubkey::new_unique();
    // share-price = 1.0 → I80F48 bit pattern = 1 << 48.
    fixture.put_bank(bank, 1i128 << 48, 1i128 << 48);

    // 1_000_000 shares (in fp48: 1_000_000 << 48) → atoms = 1_000_000.
    let shares_fp48 = 1_000_000u128 << 48;
    let result = run_and_read_result(&fixture, shares_to_amount_ix(bank, shares_fp48)).await;
    assert_eq!(result.which, 0); // shares_to_amount
    assert_eq!(result.atoms_or_shares, 1_000_000);
}

#[tokio::test]
async fn shares_to_amount_at_one_point_five_share_price() {
    let fixture = AdapterFixture::new().await;
    let bank = Pubkey::new_unique();
    // share-price = 1.5 (after some accrual)
    let asv_bits = (3i128 << 48) / 2;
    fixture.put_bank(bank, asv_bits, 1i128 << 48);

    // 1000 shares × 1.5 = 1500 atoms (no truncation since 1.5 has exact fp48 form).
    let shares_fp48 = 1000u128 << 48;
    let result = run_and_read_result(&fixture, shares_to_amount_ix(bank, shares_fp48)).await;
    assert_eq!(result.atoms_or_shares, 1500);
}

#[tokio::test]
async fn amount_to_shares_at_unit_price() {
    let fixture = AdapterFixture::new().await;
    let bank = Pubkey::new_unique();
    fixture.put_bank(bank, 1i128 << 48, 1i128 << 48);

    // amount = 7500 → shares (in fp48) = 7500 << 48.
    let result = run_and_read_result(&fixture, amount_to_shares_ix(bank, 7500)).await;
    assert_eq!(result.which, 1); // amount_to_shares
    assert_eq!(result.atoms_or_shares, 7500u128 << 48);
}

#[tokio::test]
async fn amount_to_shares_at_higher_share_price_returns_fewer_shares() {
    let fixture = AdapterFixture::new().await;
    let bank = Pubkey::new_unique();
    // share-price = 2.0 → 1000 atoms ↔ 500 shares.
    fixture.put_bank(bank, 2i128 << 48, 1i128 << 48);

    let result = run_and_read_result(&fixture, amount_to_shares_ix(bank, 1000)).await;
    // 500 shares in fp48 = 500 << 48.
    assert_eq!(result.atoms_or_shares, 500u128 << 48);
}

#[tokio::test]
async fn forged_bank_with_wrong_owner_is_rejected() {
    use marginfi_mocks::discriminator::BANK_DISCRIMINATOR;
    use marginfi_mocks::state::{Bank, BANK_ACCOUNT_SIZE};
    use marginfi_mocks::wire::WrappedI80F48;
    use solana_sdk::account::Account;

    let fixture = AdapterFixture::new().await;
    let bank_pk = Pubkey::new_unique();

    // Construct bank bytes identical to a legitimate marginfi bank...
    let mut bank: Bank = Default::default();
    bank.mint = Pubkey::new_unique();
    bank.mint_decimals = 6;
    bank.group = Pubkey::new_unique();
    bank.asset_share_value = WrappedI80F48::from_i128_bits(1i128 << 48);
    bank.liability_share_value = WrappedI80F48::from_i128_bits(1i128 << 48);
    bank.liquidity_vault = Pubkey::new_unique();
    bank.liquidity_vault_bump = 254;
    bank.liquidity_vault_authority_bump = 253;
    let mut data = Vec::with_capacity(BANK_ACCOUNT_SIZE);
    data.extend_from_slice(&BANK_DISCRIMINATOR);
    data.extend_from_slice(bytemuck::bytes_of(&bank));

    let lamports = solana_sdk::rent::Rent::default().minimum_balance(BANK_ACCOUNT_SIZE);
    // ...but owned by the System Program instead of the marginfi program.
    let acct = Account {
        lamports,
        data,
        owner: solana_program::system_program::ID,
        executable: false,
        rent_epoch: 0,
    };
    fixture
        .context
        .borrow_mut()
        .set_account(&bank_pk, &acct.into());

    // Process (not simulate) so we get a hard transaction error.
    let payer = fixture.payer.insecure_clone();
    let blockhash = fixture.context.borrow().last_blockhash;
    let tx = Transaction::new_signed_with_payer(
        &[amount_to_shares_ix(bank_pk, 1_000)],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    let result = {
        let client: RefMut<ProgramTestContext> = fixture.context.borrow_mut();
        client.banks_client.process_transaction(tx).await
    };
    assert!(
        result.is_err(),
        "adapter ix must reject a forged bank with the wrong owner; got Ok",
    );
}
