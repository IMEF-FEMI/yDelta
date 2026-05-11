//! Protocol-wide config and pause state.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use shank::ShankAccount;
use solana_program::{program_error::ProgramError, pubkey::Pubkey};
use static_assertions::const_assert_eq;

use crate::require;
use crate::validation::ydelta_checker::YdeltaAccount;

/// PDA seed for the `GlobalConfig` account.
pub const GLOBAL_CONFIG_SEED: &[u8] = b"global_config";

/// Random non-zero u64 chosen for `GlobalConfig` discriminant. ASCII
/// "ydeltaGc". Pinned at the type's introduction; do not reuse.
pub const GLOBAL_CONFIG_DISCRIMINANT: u64 = 0x79_64_65_6C_74_61_47_63;

/// `GlobalConfig` header — fits in a 128-byte account. Pre-launch we
/// can grow it; post-launch needs realloc.
pub const GLOBAL_CONFIG_SIZE: usize = 128;

#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankAccount)]
pub struct GlobalConfig {
    pub discriminator: u64, // 0..8
    /// Protocol admin. Set to `CreateGlobalConfig` payer at genesis;
    /// gates `SetGlobalPause`, `TransferProtocolAdmin`. (Future
    /// candidate to subsume per-market admin and per-vault admin.)
    pub protocol_admin: Pubkey, // 8..40
    /// Two-step transfer staging slot.
    pub pending_protocol_admin: Pubkey, // 40..72
    /// `1` blocks every state-mutating ix that takes the
    /// `global_config` account.
    pub is_paused: u8, // 72..73
    _padding: [u8; 7],      // 73..80
    /// Reserved budget. 6 × u64 = 48 bytes.
    _reserved: [u64; 6], // 80..128
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

/// Derive the `GlobalConfig` PDA. Singleton — no per-instance suffix.
pub fn global_config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[GLOBAL_CONFIG_SEED], &crate::ID)
}
