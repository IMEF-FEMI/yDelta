use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::set_fee_config::SetFeeConfigParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;

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
