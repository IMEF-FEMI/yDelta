use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use shank::ShankAccount;
use solana_program::{program_error::ProgramError, pubkey::Pubkey};
use static_assertions::const_assert_eq;

use crate::require;
use crate::validation::ydelta_checker::YdeltaAccount;

pub const GLOBAL_CONFIG_SEED: &[u8] = b"global_config";

pub const GLOBAL_CONFIG_DISCRIMINANT: u64 = 0x79_64_65_6C_74_61_47_63;

pub const GLOBAL_CONFIG_SIZE: usize = 128;

#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankAccount)]
pub struct GlobalConfig {
    pub discriminator: u64,

    pub protocol_admin: Pubkey,

    pub pending_protocol_admin: Pubkey,

    pub is_paused: u8,
    _padding: [u8; 7],

    _reserved: [u64; 6],
}
const_assert_eq!(size_of::<GlobalConfig>(), GLOBAL_CONFIG_SIZE);
const_assert_eq!(size_of::<GlobalConfig>() % 8, 0);

impl YdeltaAccount for GlobalConfig {
    fn verify_discriminant(&self) -> solana_program::entrypoint::ProgramResult {
        require!(
            self.discriminator == GLOBAL_CONFIG_DISCRIMINANT,
            ProgramError::InvalidAccountData,
            "Invalid GlobalConfig discriminant: {} (expected {})",
            self.discriminator,
            GLOBAL_CONFIG_DISCRIMINANT
        )?;
        Ok(())
    }
}

impl hypertree::Get for GlobalConfig {}

impl GlobalConfig {
    pub fn new_empty(protocol_admin: Pubkey) -> Self {
        Self {
            discriminator: GLOBAL_CONFIG_DISCRIMINANT,
            protocol_admin,
            pending_protocol_admin: Pubkey::default(),
            is_paused: 0,
            _padding: [0; 7],
            _reserved: [0; 6],
        }
    }
}

pub fn global_config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[GLOBAL_CONFIG_SEED], &crate::ID)
}
