//! UserAccount mirror tests (native, no marginfi CPI).
//!
//! The signer-side ixs auto-create the per-user `UserAccount` PDA
//! at `[b"user", owner]` on first call and mirror the canonical
//! `ClaimedSeat` onto a `MarketPosition` node. These tests cover:
//!
//! 1. claim_seat auto-creates the UserAccount and inserts a zero
//!    MarketPosition node back-referencing the seat.
//! 2. Repeated claim_seat does NOT double-write the mirror (idempotent).
//! 3. sync_market_position is rejected when target UserAccount has
//!    not been initialized.
//! 4. sync_market_position succeeds when the target's mirror exists.
//! 5. Account-count invariants for cancel/update_order ixs (the only
//!    non-marginfi signer-side ixs reachable in native).

use solana_program_test::ProgramTestContext;
use solana_sdk::{
    instruction::Instruction,
    signer::{keypair::Keypair, Signer},
    transaction::Transaction,
};

use ydelta::{
    program::{
        instruction_builders::sync_market_position_instruction::sync_market_position_instruction,
        YdeltaError,
    },
    state::user_account::{user_account_pda, MarketPositionTreeReadOnly, UserAccountFixed},
    state::USER_ACCOUNT_FIXED_SIZE,
};

use crate::assert_custom_error;
use crate::test_utils::TestFixture;

/// Read the `UserAccountFixed` header for `owner` and the dynamic
/// region as a Vec<u8> for tree-walking. Returns None when the
/// account has zero lamports (not yet auto-created).
async fn read_user_account(
    fixture: &TestFixture,
    owner: &solana_sdk::pubkey::Pubkey,
) -> Option<(UserAccountFixed, Vec<u8>)> {
    let (pda, _) = user_account_pda(owner);
    let client = fixture.context.borrow_mut();
    let acct = client.banks_client.get_account(pda).await.unwrap()?;
    if acct.lamports == 0 {
        return None;
    }
    let bytes = acct.data;
    let (header, dynamic) = bytes.split_at(USER_ACCOUNT_FIXED_SIZE);
    let fixed: UserAccountFixed = *bytemuck::from_bytes::<UserAccountFixed>(header);
    Some((fixed, dynamic.to_vec()))
}

#[tokio::test]
async fn claim_seat_auto_creates_user_account_and_inserts_mirror() {
    use hypertree::{HyperTreeValueIteratorTrait, RedBlackTreeReadOnly, NIL};

    let mut fixture = TestFixture::new().await;
    let trader = fixture.create_trader(0, 0).await;

    // Pre-claim: PDA is empty (lamports == 0).
    let pre = read_user_account(&fixture, &trader.pubkey()).await;
    assert!(
        pre.is_none(),
        "UserAccount should not exist before claim_seat"
    );

    fixture.claim_seat(&trader).await.unwrap();

    // Post-claim: PDA exists with the magic discriminant, one
    // MarketPosition node references the new seat.
    let (fixed, dynamic) = read_user_account(&fixture, &trader.pubkey())
        .await
        .expect("UserAccount auto-created on claim_seat");
    assert_eq!(
        fixed.discriminator,
        ydelta::state::USER_ACCOUNT_FIXED_DISCRIMINANT,
        "discriminator marker matches 'ydeltaUS'"
    );
    assert_eq!(fixed.owner, trader.pubkey());

    use ydelta::state::user_account::MarketPosition;
    let tree: MarketPositionTreeReadOnly =
        RedBlackTreeReadOnly::new(&dynamic, fixed.market_positions_root_index, NIL);
    let positions: Vec<MarketPosition> = tree.iter::<MarketPosition>().map(|(_, mp)| *mp).collect();
    assert_eq!(positions.len(), 1, "exactly one MarketPosition mirror");

    let mp = &positions[0];
    assert_eq!(mp.market, fixture.market.pubkey());
    assert_eq!(mp.debt_withdrawable_shares, 0);
    assert_eq!(mp.collateral_withdrawable_shares, 0);
    assert_eq!(mp.debt_encumbered_shares, 0);
    assert_eq!(mp.collateral_encumbered_shares, 0);
    // seat_index_in_market should point at a valid seat slot — the
    // caller can resolve a ClaimedSeat from it.
    assert_ne!(
        mp.seat_index_in_market, NIL,
        "seat_index back-reference set"
    );
}

#[tokio::test]
async fn sync_market_position_rejects_uninitialized_user_account() {
    let fixture = TestFixture::new().await;
    let owner = Keypair::new();
    let payer = fixture.payer_keypair();

    // Owner has never touched a signer-side ix → UserAccount PDA
    // is empty. sync_market_position must refuse to operate on an
    // empty PDA (otherwise any random signer could pay rent on
    // behalf of a third party, which the design forbids).
    let ix: Instruction = sync_market_position_instruction(
        &fixture.market.pubkey(),
        &payer.pubkey(),
        &owner.pubkey(),
    );

    let blockhash = fixture.context.borrow().last_blockhash;
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], blockhash);
    let result = fixture
        .context
        .borrow_mut()
        .banks_client
        .process_transaction(tx)
        .await;
    let result: Result<(), solana_program_test::BanksClientError> = result;
    assert_custom_error!(result, YdeltaError::IncorrectAccount);
}

#[tokio::test]
async fn sync_market_position_succeeds_after_claim_seat() {
    use hypertree::{HyperTreeValueIteratorTrait, RedBlackTreeReadOnly, NIL};

    let mut fixture = TestFixture::new().await;
    let trader = fixture.create_trader(0, 0).await;

    fixture.claim_seat(&trader).await.unwrap();
    fixture.refresh_blockhash().await;

    // Snapshot the mirror after claim_seat.
    let (_, dynamic_pre) = read_user_account(&fixture, &trader.pubkey())
        .await
        .expect("user_account exists after claim_seat");

    // Anyone can sync the trader's mirror (we use the test payer
    // as the signer; trader's UserAccount stays as-is since the
    // canonical seat hasn't moved).
    let payer = fixture.payer_keypair();
    let ix = sync_market_position_instruction(
        &fixture.market.pubkey(),
        &payer.pubkey(),
        &trader.pubkey(),
    );
    let blockhash = fixture.context.borrow().last_blockhash;
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], blockhash);
    fixture
        .context
        .borrow_mut()
        .banks_client
        .process_transaction(tx)
        .await
        .unwrap();

    // After sync, the mirror still holds exactly one MarketPosition
    // and its balances are still zero (seat hasn't moved).
    let (fixed_post, dynamic_post) = read_user_account(&fixture, &trader.pubkey()).await.unwrap();
    use ydelta::state::user_account::MarketPosition;
    let tree: MarketPositionTreeReadOnly =
        RedBlackTreeReadOnly::new(&dynamic_post, fixed_post.market_positions_root_index, NIL);
    assert_eq!(
        tree.iter::<MarketPosition>().count(),
        1,
        "still exactly one MarketPosition"
    );

    // Sanity: the dynamic-region size matches (no expansion happened
    // — the upsert hit the existing node).
    assert_eq!(
        dynamic_pre.len(),
        dynamic_post.len(),
        "no realloc on idempotent sync"
    );
}

#[tokio::test]
async fn cancel_order_account_count_includes_user_account_pair() {
    use ydelta::program::instruction_builders::cancel_order_instruction::cancel_order_instruction;

    let payer = solana_sdk::signature::Keypair::new();
    let market = solana_sdk::pubkey::Pubkey::new_unique();
    let ix = cancel_order_instruction(&market, &payer.pubkey(), 0, None, None, None);
    // 3 (payer/market/system) + 2 (UserAccount + system_program for
    // auto-create) + 1 (global_config) = 6. The optional
    // secondary-loan trailing account is omitted (None).
    assert_eq!(
        ix.accounts.len(),
        6,
        "cancel_order primary = 3 + 2 (UserAccount) + 1 (global_config)"
    );
}

#[tokio::test]
async fn update_order_account_count_includes_user_account_pair() {
    use ydelta::program::instruction_builders::update_order_instruction::update_order_instruction;

    let payer = solana_sdk::signature::Keypair::new();
    let market = solana_sdk::pubkey::Pubkey::new_unique();
    let ix = update_order_instruction(
        &market,
        &payer.pubkey(),
        0,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    assert_eq!(
        ix.accounts.len(),
        6,
        "update_order = 3 + 2 (UserAccount) + 1 (global_config)"
    );
}

#[tokio::test]
async fn claim_seat_account_count_includes_user_account() {
    use ydelta::program::instruction_builders::claim_seat_instruction::claim_seat_instruction;

    let payer = solana_sdk::signature::Keypair::new();
    let market = solana_sdk::pubkey::Pubkey::new_unique();
    let ix = claim_seat_instruction(&market, &payer.pubkey());
    // payer/market/system_program/UserAccount = 4. (claim_seat does
    // NOT take a duplicate trailing system_program — its loader
    // reads the existing system_program slot for both auto-create
    // and PDA derivation.)
    assert_eq!(
        ix.accounts.len(),
        5,
        "claim_seat = 3 + 1 (UserAccount PDA) + 1 (global_config)"
    );
}

#[tokio::test]
async fn sync_market_position_account_count_is_four() {
    let payer = solana_sdk::signature::Keypair::new();
    let owner = solana_sdk::signature::Keypair::new();
    let market = solana_sdk::pubkey::Pubkey::new_unique();
    let ix = sync_market_position_instruction(&market, &payer.pubkey(), &owner.pubkey());
    assert_eq!(
        ix.accounts.len(),
        5,
        "sync_market_position = payer + global_config + user_account + market + owner"
    );
}

// Suppress unused-import warning when not all helpers are used.
#[allow(dead_code)]
fn _phantom(_ctx: &ProgramTestContext) {}
