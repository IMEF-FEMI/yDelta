//! Byte-layout constants and discriminants shared by every account type
//! in `state`. The market, user-account, and global-vault accounts each
//! pack a fixed header followed by a hypertree of fixed-size nodes; the
//! sizes here pin those block layouts so size mismatches fail at compile
//! time via `const_assert_eq!` in the type definitions.

use hypertree::RBTREE_OVERHEAD_BYTES;

/// Byte size of [`super::market::MarketFixed`], the market-account header
/// that sits before the dynamic node region.
pub const MARKET_FIXED_SIZE: usize = 512;

/// Per-node block size for the market account's red-black-tree region.
/// `RestingOrder`, `ClaimedSeat`, and `MatchedLoan` all fit one per block.
pub const MARKET_BLOCK_SIZE: usize = 160;

const MARKET_BLOCK_PAYLOAD_SIZE: usize = MARKET_BLOCK_SIZE - RBTREE_OVERHEAD_BYTES;

/// Payload byte size of a [`super::resting_order::RestingOrder`] node.
pub const RESTING_ORDER_SIZE: usize = MARKET_BLOCK_PAYLOAD_SIZE;
/// Payload byte size of a [`super::claimed_seat::ClaimedSeat`] node.
pub const CLAIMED_SEAT_SIZE: usize = MARKET_BLOCK_PAYLOAD_SIZE;

/// Payload byte size of a [`super::market::MatchedLoan`] node.
pub const MATCHED_LOAN_SIZE: usize = MARKET_BLOCK_PAYLOAD_SIZE;

const FREE_LIST_OVERHEAD: usize = 4;
/// Byte size of a free-list padding block on a market account.
pub const MARKET_FREE_LIST_BLOCK_SIZE: usize = MARKET_BLOCK_SIZE - FREE_LIST_OVERHEAD;

/// `last_valid_unix_ts` sentinel meaning "no expiry"; orders with this
/// value never expire.
pub const NO_EXPIRATION_LAST_VALID_UNIX_TS: i64 = 0;

/// Eight-byte tag at the head of every market account; identifies the
/// account type to the loader.
pub const MARKET_FIXED_DISCRIMINANT: u64 = 0x79_64_65_6C_74_61_4D_4B;

/// Layout version stamped onto every fixed account header; loaders reject
/// any account whose `version` field does not match.
pub const ACCOUNT_LAYOUT_VERSION: u8 = 1;

/// Smallest principal that a borrower bid will accept; defended by
/// `match_borrower_bid` before any seat/order work.
pub const MIN_PRINCIPAL_ATOMS: u64 = 1;

/// Byte size of [`super::user_account::UserAccountFixed`].
pub const USER_ACCOUNT_FIXED_SIZE: usize = 128;

/// Eight-byte tag at the head of every user account.
pub const USER_ACCOUNT_FIXED_DISCRIMINANT: u64 = 0x79_64_65_6C_74_61_55_53;

/// Per-node block size for the user-account dynamic region.
pub const USER_ACCOUNT_BLOCK_SIZE: usize = MARKET_BLOCK_SIZE;
/// Payload byte size of a user-account node; the three node types
/// (`VaultPosition`, `MarketPosition`, `UserLoanRef`) all match this.
pub const USER_ACCOUNT_BLOCK_PAYLOAD_SIZE: usize = USER_ACCOUNT_BLOCK_SIZE - RBTREE_OVERHEAD_BYTES;
/// Byte size of a free-list padding block on a user account.
pub const USER_ACCOUNT_FREE_LIST_BLOCK_SIZE: usize = USER_ACCOUNT_BLOCK_SIZE - FREE_LIST_OVERHEAD;

/// Payload byte size of a [`super::user_account::VaultPosition`].
pub const VAULT_POSITION_SIZE: usize = USER_ACCOUNT_BLOCK_PAYLOAD_SIZE;
/// Payload byte size of a [`super::user_account::MarketPosition`].
pub const MARKET_POSITION_SIZE: usize = USER_ACCOUNT_BLOCK_PAYLOAD_SIZE;
/// Payload byte size of a [`super::user_account::UserLoanRef`].
pub const USER_LOAN_REF_SIZE: usize = USER_ACCOUNT_BLOCK_PAYLOAD_SIZE;

/// Byte size of [`super::vault::GlobalVaultFixed`].
pub const GLOBAL_VAULT_FIXED_SIZE: usize = 320;

/// Eight-byte tag at the head of every global vault account.
pub const GLOBAL_VAULT_FIXED_DISCRIMINANT: u64 = 0x79_64_65_6C_74_61_56_61;

/// Per-node block size for the global vault's risk-profile region.
/// `RiskProfile` payloads are large enough to need their own block size.
pub const RISK_PROFILE_BLOCK_SIZE: usize = 512;
/// Payload byte size of a [`super::vault::RiskProfile`] node.
pub const RISK_PROFILE_BLOCK_PAYLOAD_SIZE: usize = RISK_PROFILE_BLOCK_SIZE - RBTREE_OVERHEAD_BYTES;
/// Byte size of a free-list padding block in the risk-profile region.
pub const RISK_PROFILE_FREE_LIST_BLOCK_SIZE: usize = RISK_PROFILE_BLOCK_SIZE - FREE_LIST_OVERHEAD;
/// Alias for [`RISK_PROFILE_BLOCK_PAYLOAD_SIZE`].
pub const RISK_PROFILE_SIZE: usize = RISK_PROFILE_BLOCK_PAYLOAD_SIZE;

/// Per-node block size for the global vault's auxiliary node region
/// (depositor seats and order refs).
pub const VAULT_NODE_BLOCK_SIZE: usize = MARKET_BLOCK_SIZE;
/// Payload byte size of an auxiliary vault node.
pub const VAULT_NODE_BLOCK_PAYLOAD_SIZE: usize = VAULT_NODE_BLOCK_SIZE - RBTREE_OVERHEAD_BYTES;
/// Byte size of a free-list padding block in the vault-node region.
pub const VAULT_NODE_FREE_LIST_BLOCK_SIZE: usize = VAULT_NODE_BLOCK_SIZE - FREE_LIST_OVERHEAD;
/// Payload byte size of a [`super::vault::RiskProfileDepositorSeat`].
pub const VAULT_CLAIMED_SEAT_SIZE: usize = VAULT_NODE_BLOCK_PAYLOAD_SIZE;
/// Payload byte size of a [`super::vault::RiskProfileOrderRef`].
pub const VAULT_ORDER_REF_SIZE: usize = VAULT_NODE_BLOCK_PAYLOAD_SIZE;
