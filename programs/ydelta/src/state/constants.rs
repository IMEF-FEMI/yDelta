use hypertree::RBTREE_OVERHEAD_BYTES;

/// Header size of a `MarketFixed`. 512 bytes leaves headroom for
/// future fields without on-disk migrations.
pub const MARKET_FIXED_SIZE: usize = 512;

/// Block size for every tree-node payload allocated inside the
/// market's dynamic region. RestingOrder, ClaimedSeat, and MatchedLoan
/// all share this size so they coexist in one free list.
///
/// Sized at 160 (not 128) so `MatchedLoan` has room for two u128
/// share-price snapshots (debt + collateral banks). Other payloads use
/// the extra 32 bytes as future-fields headroom.
pub const MARKET_BLOCK_SIZE: usize = 160;

const MARKET_BLOCK_PAYLOAD_SIZE: usize = MARKET_BLOCK_SIZE - RBTREE_OVERHEAD_BYTES;

pub const RESTING_ORDER_SIZE: usize = MARKET_BLOCK_PAYLOAD_SIZE;
pub const CLAIMED_SEAT_SIZE: usize = MARKET_BLOCK_PAYLOAD_SIZE;
/// Sized identically to the other payloads so it can reuse the shared
/// free list.
pub const MATCHED_LOAN_SIZE: usize = MARKET_BLOCK_PAYLOAD_SIZE;

const FREE_LIST_OVERHEAD: usize = 4;
pub const MARKET_FREE_LIST_BLOCK_SIZE: usize = MARKET_BLOCK_SIZE - FREE_LIST_OVERHEAD;

/// Wall-clock sentinel for `RestingOrder.last_valid_unix_ts` meaning
/// "never expires". Mirrors manifest's `NO_EXPIRATION_LAST_VALID_SLOT`
/// pattern but in unix-ts since yDelta orders are time-windowed.
pub const NO_EXPIRATION_LAST_VALID_UNIX_TS: i64 = 0;

/// Discriminant prefix of an initialised `MarketFixed`. Random non-zero u64
/// chosen at the project's inception; do not reuse for any other type.
pub const MARKET_FIXED_DISCRIMINANT: u64 = 0x79_64_65_6C_74_61_4D_4B; // "ydeltaMK"

/// Dust gate. Caller-supplied principal must be at least this many
/// atoms.
pub const MIN_PRINCIPAL_ATOMS: u64 = 1;

/// `UserAccountFixed` header size. 128 bytes leaves headroom for
/// future fields so on-disk layout doesn't migrate.
pub const USER_ACCOUNT_FIXED_SIZE: usize = 128;

/// Discriminant prefix of an initialised `UserAccountFixed`. Random
/// non-zero u64 chosen at the project's inception; do not reuse for
/// any other type.
pub const USER_ACCOUNT_FIXED_DISCRIMINANT: u64 = 0x79_64_65_6C_74_61_55_53; // "ydeltaUS"

/// Payload size for tree-node entries on a `UserAccount`. Re-uses
/// the same `MARKET_BLOCK_SIZE` (160 B) block as the market's shared
/// free list, so all three node types (`VaultPosition`,
/// `MarketPosition`, `UserLoanRef`) are exactly
/// `USER_ACCOUNT_BLOCK_PAYLOAD_SIZE` bytes to share one free list.
/// Same allocator semantics as `RestingOrder` / `ClaimedSeat` /
/// `MatchedLoan`.
pub const USER_ACCOUNT_BLOCK_SIZE: usize = MARKET_BLOCK_SIZE;
pub const USER_ACCOUNT_BLOCK_PAYLOAD_SIZE: usize = USER_ACCOUNT_BLOCK_SIZE - RBTREE_OVERHEAD_BYTES;
pub const USER_ACCOUNT_FREE_LIST_BLOCK_SIZE: usize = USER_ACCOUNT_BLOCK_SIZE - FREE_LIST_OVERHEAD;

pub const VAULT_POSITION_SIZE: usize = USER_ACCOUNT_BLOCK_PAYLOAD_SIZE;
pub const MARKET_POSITION_SIZE: usize = USER_ACCOUNT_BLOCK_PAYLOAD_SIZE;
pub const USER_LOAN_REF_SIZE: usize = USER_ACCOUNT_BLOCK_PAYLOAD_SIZE;

// ─────────────────── `GlobalVault` ───────────────────

/// `GlobalVaultFixed` header size. 320 bytes covers the current
/// fields plus reserved budget. Pre-launch growth is safe; once
/// vaults are deployed, further growth requires a realloc migration.
pub const GLOBAL_VAULT_FIXED_SIZE: usize = 320;

/// Discriminant prefix of an initialised `GlobalVaultFixed`. Random
/// non-zero u64 chosen at the project's inception; do not reuse for
/// any other type. ASCII "ydeltaVa".
pub const GLOBAL_VAULT_FIXED_DISCRIMINANT: u64 = 0x79_64_65_6C_74_61_56_61;

/// `RiskProfile` block size. Larger than the standard 128 because
/// `RiskProfile` carries an inline `[Pubkey; 8]` array of allowed
/// markets (256 bytes alone). Profiles get their own free list on
/// the vault.
pub const RISK_PROFILE_BLOCK_SIZE: usize = 512;
pub const RISK_PROFILE_BLOCK_PAYLOAD_SIZE: usize = RISK_PROFILE_BLOCK_SIZE - RBTREE_OVERHEAD_BYTES;
pub const RISK_PROFILE_FREE_LIST_BLOCK_SIZE: usize = RISK_PROFILE_BLOCK_SIZE - FREE_LIST_OVERHEAD;
pub const RISK_PROFILE_SIZE: usize = RISK_PROFILE_BLOCK_PAYLOAD_SIZE;

/// Vault-side small-node allocator block size. Used by
/// `RiskProfileDepositorSeat` and `RiskProfileOrderRef`. Same size as
/// `MARKET_BLOCK_SIZE` (160 B) and shares one free list with the
/// rest of the vault's smaller node types.
pub const VAULT_NODE_BLOCK_SIZE: usize = MARKET_BLOCK_SIZE;
pub const VAULT_NODE_BLOCK_PAYLOAD_SIZE: usize = VAULT_NODE_BLOCK_SIZE - RBTREE_OVERHEAD_BYTES;
pub const VAULT_NODE_FREE_LIST_BLOCK_SIZE: usize = VAULT_NODE_BLOCK_SIZE - FREE_LIST_OVERHEAD;
pub const VAULT_CLAIMED_SEAT_SIZE: usize = VAULT_NODE_BLOCK_PAYLOAD_SIZE;
pub const VAULT_ORDER_REF_SIZE: usize = VAULT_NODE_BLOCK_PAYLOAD_SIZE;

/// Hard upper bound on a profile's `allowed_market_max`. The
/// `create_risk_profile` ix enforces this ceiling against
/// caller-supplied `allowed_market_max` values.
pub const RISK_PROFILE_ALLOWED_MARKETS_CAP: u8 = 8;
