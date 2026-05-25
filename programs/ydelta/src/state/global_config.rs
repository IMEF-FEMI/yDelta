//! Singleton `GlobalConfig` account that holds protocol-wide settings:
//! the protocol admin pubkey, its pending-handoff slot, and the global
//! pause switch consulted by every state-mutating instruction.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use shank::ShankAccount;
use solana_program::{program_error::ProgramError, pubkey::Pubkey};
use static_assertions::const_assert_eq;

use crate::require;
use crate::validation::ydelta_checker::YdeltaAccount;

/// PDA seed for the singleton [`GlobalConfig`] account.
pub const GLOBAL_CONFIG_SEED: &[u8] = b"global_config";

/// Eight-byte tag at the head of the global-config account.
pub const GLOBAL_CONFIG_DISCRIMINANT: u64 = 0x79_64_65_6C_74_61_47_63;

/// Byte size of [`GlobalConfig`].
pub const GLOBAL_CONFIG_SIZE: usize = 128;

/// Singleton account holding protocol-wide configuration: the admin
/// pubkey, its pending-transfer slot, and the protocol-wide pause flag.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankAccount)]
pub struct GlobalConfig {
    /// Layout tag; must equal [`GLOBAL_CONFIG_DISCRIMINANT`].
    pub discriminator: u64,

    /// Current protocol admin. Signs all admin-only instructions.
    pub protocol_admin: Pubkey,

    /// Proposed next admin; takes effect via `AcceptProtocolAdmin`.
    pub pending_protocol_admin: Pubkey,

    /// Global pause switch. When non-zero, instructions that guard on it
    /// reject with `Paused`.
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
    /// Build a fresh config seeded with `protocol_admin`, no pending
    /// admin, and the pause switch off.
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

/// Derives the singleton [`GlobalConfig`] PDA and its bump.
pub fn global_config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[GLOBAL_CONFIG_SEED], &crate::ID)
}
