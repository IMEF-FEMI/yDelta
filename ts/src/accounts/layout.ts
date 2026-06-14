// Authoritative byte-offset table for the fixed-size headers in the
// yDelta program. These mirror the Rust `#[repr(C)]` structs:
//   - MarketFixed       (programs/ydelta/src/state/market.rs)
//   - FeeConfig         (programs/ydelta/src/state/market.rs, nested in MarketFixed)
//   - GlobalConfig      (programs/ydelta/src/state/global_config.rs)
//   - GlobalVaultFixed  (programs/ydelta/src/state/vault.rs)
//
// External readers (the UI, crankers, future on-chain programs
// filtering ydelta accounts via memcmp/getProgramAccounts) read these as raw bytes and
// need stable offsets. Keeping them in one file means a layout change
// updates a single constant rather than every magic number scattered
// across decoders.
//
// Run `pnpm test` (or `yarn vitest`) and the decoder tests will catch
// drift — they assert the decoded shape end-to-end against a fixture
// buffer.

// ─────────────────────────────────────────────────────────────────────
// MarketFixed (512 bytes total)
// ─────────────────────────────────────────────────────────────────────

export const MarketFixedOffsets = {
  DISCRIMINATOR: 0,
  VERSION: 8,
  DEBT_MINT_DECIMALS: 9,
  COLLATERAL_MINT_DECIMALS: 10,
  DEBT_VAULT_BUMP: 11,
  COLLATERAL_VAULT_BUMP: 12,
  DEBT_MINT: 16,
  COLLATERAL_MINT: 48,
  DEBT_VAULT: 80,
  COLLATERAL_VAULT: 112,
  ACCUMULATED_PROTOCOL_FEE_SHARES: 144,
  ORDER_SEQUENCE_NUMBER: 160,
  MATCHED_LOAN_SEQUENCE: 168,
  NUM_BYTES_ALLOCATED: 176,
  ASKS_ROOT_INDEX: 188,
  ASKS_BEST_INDEX: 192,
  CLAIMED_SEATS_ROOT_INDEX: 196,
  MATCHED_LOANS_ROOT_INDEX: 200,
  FREE_LIST_HEAD_INDEX: 204,
  POSITION_COUNT: 208,
  FEE_CONFIG: 212,
  LENDER_INTEGRATION_ACCOUNT: 240,
  BORROWER_INTEGRATION_ACCOUNT: 272,
  DEBT_LENDING_POOL: 304,
  COLLATERAL_LENDING_POOL: 336,
  MARGINFI_GROUP: 368,
  MARKET_SIGNER: 400,
  MARKET_SIGNER_BUMP: 432,
  LENDER_INTEGRATION_ACCOUNT_BUMP: 433,
  BORROWER_INTEGRATION_ACCOUNT_BUMP: 434,
  ADMIN: 440,
  PENDING_ADMIN: 472,
  IS_PAUSED: 504,
} as const;

// ─────────────────────────────────────────────────────────────────────
// FeeConfig (24 bytes, nested at MarketFixed.FEE_CONFIG)
// ─────────────────────────────────────────────────────────────────────

export const FeeConfigOffsets = {
  // v1 (D3b/D17): curator_fee_bps moved to the sub-vault and ltv_buffer_bps
  // was removed, so the liquidation fields shifted up to 6/8.
  PROTOCOL_FEE_BPS_FLOOR: 0,
  ORIGINATION_BPS: 2,
  CURATOR_SPLIT_BPS: 4,
  LIQUIDATION_KEEPER_BPS: 6,
  LIQUIDATION_PROTOCOL_BPS: 8,
  GRACE_PERIOD_SECONDS: 16,
} as const;

// ─────────────────────────────────────────────────────────────────────
// GlobalConfig (128 bytes total)
// ─────────────────────────────────────────────────────────────────────

export const GlobalConfigOffsets = {
  DISCRIMINATOR: 0,
  PROTOCOL_ADMIN: 8,
  PENDING_PROTOCOL_ADMIN: 40,
  IS_PAUSED: 72,
} as const;

// ─────────────────────────────────────────────────────────────────────
// GlobalVaultFixed (320 bytes total)
// ─────────────────────────────────────────────────────────────────────

export const GlobalVaultFixedOffsets = {
  DISCRIMINATOR: 0,
  MINT: 8,
  GLOBAL_VAULT_ADMIN: 40,
  INTEGRATION_POOL: 72,
  INTEGRATION_ACCOUNT: 104,
  GLOBAL_VAULT_SIGNER: 136,
  LENDING_POOL: 168,
  PENDING_GLOBAL_VAULT_ADMIN: 240,
  IS_PAUSED: 304,
} as const;

// ─────────────────────────────────────────────────────────────────────
// RestingOrder (144 bytes — bytemuck Pod tree-node payload)
//
// Mirrors `#[repr(C)] RestingOrder` (programs/ydelta/src/state/resting_order.rs).
// `ltv_buffer_bps` (borrower LTV buffer) was added at offset 66, taken from
// the leading 2 bytes of the old 6-byte `_pad2`; total size stays 144.
// Decoder lives at ts/src/accounts/restingOrder.ts.
// ─────────────────────────────────────────────────────────────────────

export const RestingOrderOffsets = {
  TRADER_SEAT_INDEX: 0,
  SEQUENCE_NUMBER: 8,
  PRINCIPAL_ATOMS: 16,
  COLLATERAL_ATOMS: 24,
  LAST_VALID_UNIX_TS: 32,
  TERM_SECONDS: 40,
  RATE_BPS: 44,
  SIDE: 46,
  ORDER_TYPE: 47,
  FLAGS: 48,
  SHARE_PRICE_SNAPSHOT_FP48: 50,
  LTV_BUFFER_BPS: 66,
} as const;
