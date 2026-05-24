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

    let _ = YdeltaAccountInfo::<MarketFixed>::new(market_ai)?;

    let (expected_user_account, _bump) = user_account_pda(owner_ai.key);
    require!(
        *owner_user_account_ai.key == expected_user_account,
        YdeltaError::IncorrectAccount,
        "owner_user_account does not match user_account_pda(owner) = [b\"user\", owner]"
    )?;

    require!(
        owner_user_account_ai.lamports() > 0,
        YdeltaError::IncorrectAccount,
        "owner_user_account is not initialized; the owner must touch a \
         signer-side ix first to auto-create it"
    )?;

    let _ = YdeltaAccountInfo::<UserAccountFixed>::new(owner_user_account_ai)?;

    super::shared::sync_signer_market_position(market_ai, owner_user_account_ai, owner_ai.key)?;

    Ok(())
}
