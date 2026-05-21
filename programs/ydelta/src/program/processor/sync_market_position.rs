//! Refresh a user's `MarketPosition` mirror from the canonical seat.

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    pubkey::Pubkey,
};

use crate::program::YdeltaError;
use crate::require;
use crate::state::user_account::{user_account_pda, UserAccountFixed};
use crate::state::MarketFixed;
use crate::validation::loaders::load_global_config;
use crate::validation::{Signer, YdeltaAccountInfo};

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct SyncMarketPositionParams {}

pub fn process_sync_market_position(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let mut iter = accounts.iter();

    let _payer = Signer::new_payer(next_account_info(&mut iter)?)?;
    let _ = load_global_config(&mut iter)?;
    let owner_user_account_ai = next_account_info(&mut iter)?;
    let market_ai = next_account_info(&mut iter)?;
    let owner_ai = next_account_info(&mut iter)?;

    // Validate owner + discriminator on `market_ai`. Without this check
    // any yDelta-owned account whose first bytes happen to deserialize
    // as `MarketFixed` (a different market, a Loan PDA, etc.) could be
    // passed in — the processor would then read a seat_index out of
    // those spoofed bytes and write a MarketPosition row onto the
    // victim's mirror.
    let _ = YdeltaAccountInfo::<MarketFixed>::new(market_ai)?;

    // 1. Verify `owner_user_account_ai` is the canonical PDA for `owner`.
    let (expected_user_account, _bump) = user_account_pda(owner_ai.key);
    require!(
        *owner_user_account_ai.key == expected_user_account,
        YdeltaError::IncorrectAccount,
        "owner_user_account does not match user_account_pda(owner) = [b\"user\", owner]"
    )?;

    // 2. Refuse to operate on a non-initialized UserAccount. Anyone
    //    signing as payer would otherwise be able to top up rent on
    //    a third party's PDA — undesirable. Owner must initialize
    //    via a self-signed signer-side ix (deposit / claim_seat /
    //    place_order / etc.) first.
    require!(
        owner_user_account_ai.lamports() > 0,
        YdeltaError::IncorrectAccount,
        "owner_user_account is not initialized; the owner must touch a \
         signer-side ix first to auto-create it"
    )?;
    // Verify owner + discriminant + version, consistent with every other
    // loader (subsumes the data-empty check).
    let _ = YdeltaAccountInfo::<UserAccountFixed>::new(owner_user_account_ai)?;

    // 3. Reuse the same helper signer-side ixs use; it handles read
    //    (canonical seat) → upsert (MarketPosition) → write balances.
    super::shared::sync_signer_market_position(market_ai, owner_user_account_ai, owner_ai.key)?;

    Ok(())
}
