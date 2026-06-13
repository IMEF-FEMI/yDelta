//! Builders for the two sub-vault creation instructions:
//! `CreatePoolSubVault` (protocol-admin assigns a curator + fee) and
//! `CreatePrivateSubVault` (permissionless; signer becomes the curator
//! and sole depositor, fee = 0). The processor auto-assigns a monotonic
//! `sub_vault_id` starting at 1 (0 is the sentinel/invalid id).

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::create_sub_vault::{
    CreatePoolSubVaultParams, CreatePrivateSubVaultParams,
};
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

fn sub_vault_create_accounts(payer: &Pubkey, vault: Pubkey) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new(vault, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ]
}

/// Builds `CreatePoolSubVault` for the vault keyed by `bank`. `payer`
/// (signer) must be `GlobalConfig.protocol_admin`; `curator` is stamped
/// as the pool's curator and `curator_fee_bps` (≤ `MAX_CURATOR_FEE_BPS`)
/// is immutable after creation.
#[allow(clippy::too_many_arguments)]
pub fn create_pool_sub_vault_instruction(
    bank: &Pubkey,
    payer: &Pubkey,
    curator: &Pubkey,
    spread_bps: u16,
    max_ltv_bps: u16,
    liquidation_ltv_bps: u16,
    max_term_seconds: u32,
    curator_fee_bps: u16,
) -> Instruction {
    let (vault, _) = global_vault_pda(bank);

    let mut data = YdeltaInstruction::CreatePoolSubVault.to_vec();
    CreatePoolSubVaultParams {
        curator: *curator,
        spread_bps,
        max_ltv_bps,
        liquidation_ltv_bps,
        max_term_seconds,
        curator_fee_bps,
    }
    .serialize(&mut data)
    .unwrap();

    Instruction {
        program_id: crate::id(),
        accounts: sub_vault_create_accounts(payer, vault),
        data,
    }
}

/// Builds `CreatePrivateSubVault` for the vault keyed by `bank`. Any
/// signer may call; the signer becomes the curator/sole depositor.
pub fn create_private_sub_vault_instruction(
    bank: &Pubkey,
    payer: &Pubkey,
    spread_bps: u16,
    max_ltv_bps: u16,
    liquidation_ltv_bps: u16,
    max_term_seconds: u32,
) -> Instruction {
    let (vault, _) = global_vault_pda(bank);

    let mut data = YdeltaInstruction::CreatePrivateSubVault.to_vec();
    CreatePrivateSubVaultParams {
        spread_bps,
        max_ltv_bps,
        liquidation_ltv_bps,
        max_term_seconds,
    }
    .serialize(&mut data)
    .unwrap();

    Instruction {
        program_id: crate::id(),
        accounts: sub_vault_create_accounts(payer, vault),
        data,
    }
}
