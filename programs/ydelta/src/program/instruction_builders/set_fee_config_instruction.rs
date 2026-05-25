//! Builds the `YdeltaInstruction::SetFeeConfig` instruction: market-admin
//! patches `MarketFixed.fee_config` (each `Some` field in `params`
//! overrides, `None` leaves the existing value).

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::set_fee_config::SetFeeConfigParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;

/// Builds the `SetFeeConfig` instruction for `market`. `admin` (signer)
/// must equal the market admin; `params` carries the partial overrides.
#[allow(clippy::too_many_arguments)]
pub fn set_fee_config_instruction(
    market: &Pubkey,
    admin: &Pubkey,
    params: SetFeeConfigParams,
) -> Instruction {
    let mut data = YdeltaInstruction::SetFeeConfig.to_vec();
    params.serialize(&mut data).unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*admin, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}
