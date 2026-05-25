//! Account-validation layer. Each per-account-shape checker (signer,
//! mint, token account, marginfi bank, etc) returns a typed wrapper
//! that proves the underlying `AccountInfo` passed its owner / layout
//! / discriminant checks. [`loaders`] is the single source of truth
//! for per-instruction account-list shapes — every processor calls
//! exactly one `XContext::load`.

pub mod loaders;
pub mod marginfi_checkers;
pub mod pdas;
pub mod solana_checkers;
pub mod token_checkers;
pub mod user_account;
pub mod ydelta_checker;

pub use marginfi_checkers::*;
pub use pdas::*;
pub use solana_checkers::*;
pub use token_checkers::*;
pub use user_account::*;
pub use ydelta_checker::*;
