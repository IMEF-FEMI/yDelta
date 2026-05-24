#![allow(clippy::too_many_arguments)]

use solana_program::{declare_id, pubkey::Pubkey};

declare_id!("MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA");

pub mod cpi;
pub mod discriminator;
pub mod state;
pub mod wire;

pub use cpi::*;
pub use discriminator::*;
pub use state::*;
pub use wire::*;

pub const MARGINFI_PROGRAM_ID: Pubkey = ID;
