//! `marginfi-mocks` — slim vendored wire-format types and CPI ix builders for
//! marginfi v0.1.8 (the version currently deployed on mainnet at
//! `MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA`).
//!
//! yDelta's `MarginfiV18Adapter` uses this crate to construct cross-program
//! invocations into marginfi WITHOUT taking marginfi-v2 as a dependency.
//! Reasons (per project decisions):
//! - Marginfi pulls a heavy dep tree that fights yDelta's own toolchain pins.
//! - Future protocol adapters (Kamino, Drift, …) follow the same vendor-then-CPI
//!   pattern; conditioning yDelta on every upstream protocol's crates would
//!   bloat the build and make solana-version upgrades a multi-protocol
//!   coordination problem.
//!
//! Pattern lifted from how marginfi itself integrates Kamino/Drift/Solend/
//! JupLend (see `reference/marginfi-v2/programs/{kamino,drift,solend,juplend}-mocks/`).
//!
//! Source of truth for everything in this crate: the on-chain v0.1.8 IDL at
//! `programs/ydelta/tests/fixtures/marginfi.idl.json`. If marginfi mainnet
//! upgrades and we want to track, re-run `scripts/fetch-marginfi-fixture.sh`
//! and re-derive these constants.

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

/// Re-export the marginfi program ID with a stable name. Adapter code uses
/// this to validate `program_id` accounts.
pub const MARGINFI_PROGRAM_ID: Pubkey = ID;
