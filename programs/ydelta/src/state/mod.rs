//! On-chain account data types and the helpers that mutate them.
//!
//! Each submodule owns one logical account or value type:
//! market order book (`market`, `market_helpers`, `resting_order`),
//! per-trader seats (`claimed_seat`), settled loans (`loan`, `ltv`),
//! global lending vaults (`vault`), and per-user position trackers
//! (`user_account`). `constants` carries the layout sizes and
//! discriminants shared across these accounts.

pub mod claimed_seat;
pub mod constants;
pub mod dynamic_account;
pub mod global_config;
pub mod loan;
pub mod ltv;
pub mod market;
pub mod market_helpers;
pub mod resting_order;
pub mod user_account;
pub mod utils;
pub mod vault;

pub use claimed_seat::*;
pub use constants::*;
pub use dynamic_account::*;
pub use global_config::*;
pub use loan::*;
pub use market::*;
pub use market_helpers::*;
pub use resting_order::*;
pub use user_account::*;
pub use utils::*;
pub use vault::*;
