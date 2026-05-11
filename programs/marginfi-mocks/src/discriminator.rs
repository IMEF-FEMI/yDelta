//! Anchor-style 8-byte discriminators for marginfi v0.1.8 instructions and
//! account headers. Values copied verbatim from the on-chain IDL at
//! `programs/ydelta/tests/fixtures/marginfi.idl.json` (version 0.1.8).
//!
//! Hardcoded as `[u8; 8]` constants rather than computed via `keccak`/`sha256`
//! to match marginfi's own vendoring pattern (see `kamino-mocks/src/state.rs:14`)
//! and to keep CU cost zero at the call site.

// ─────────────────── Account discriminators ───────────────────

pub const BANK_DISCRIMINATOR: [u8; 8] = [142, 49, 166, 242, 50, 66, 97, 188];
pub const MARGINFI_ACCOUNT_DISCRIMINATOR: [u8; 8] = [67, 178, 130, 109, 126, 114, 28, 42];
pub const MARGINFI_GROUP_DISCRIMINATOR: [u8; 8] = [182, 23, 173, 240, 151, 206, 182, 67];

// ─────────────────── Instruction discriminators ───────────────────
//
// The four lending-flow ixs we CPI into. Anchor prepends each ix's data
// payload with these 8 bytes.

pub const IX_LENDING_ACCOUNT_DEPOSIT: [u8; 8] = [171, 94, 235, 103, 82, 64, 212, 140];
pub const IX_LENDING_ACCOUNT_WITHDRAW: [u8; 8] = [36, 72, 74, 19, 210, 210, 192, 192];
pub const IX_LENDING_ACCOUNT_BORROW: [u8; 8] = [4, 126, 116, 53, 48, 5, 212, 31];
pub const IX_LENDING_ACCOUNT_REPAY: [u8; 8] = [79, 209, 172, 177, 222, 51, 173, 151];

/// Bootstrap ix: opens a marginfi account under a marginfi group.
pub const IX_MARGINFI_ACCOUNT_INITIALIZE: [u8; 8] = [43, 78, 61, 255, 148, 52, 249, 154];
