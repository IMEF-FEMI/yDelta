/**
 * `SubVault` (496 bytes) + `SubVaultDepositorSeat` (144 bytes) +
 * `SubVaultOrderRef` (144 bytes) — the three tree-node payloads in a
 * `GlobalVault`'s dynamic region. SubVault lives in the 512-byte free
 * list; the other two share the 160-byte free list.
 */
import { PublicKey } from '@solana/web3.js';

import { readI64, readPubkey, readU128, readU16, readU32, readU64, readU8 } from './_read.js';

export const SUB_VAULT_SIZE = 496;
export const SUB_VAULT_DEPOSITOR_SEAT_SIZE = 144;
export const SUB_VAULT_ORDER_REF_SIZE = 144;

/** `SubVault.kind` discriminant (state/vault.rs). */
export enum SubVaultKind {
  /** Curator-run pool, multiple depositors. */
  Pool = 0,
  /** Single-owner sub-vault — owner is the curator and only allowed depositor; fee forced to 0. */
  Private = 1,
}

/* ── SubVault ─────────────────────────────────────────────── */

/**
 * Layout (offsets from state/vault.rs):
 *   @0   2    u16    sub_vault_id            (1-based; 0 is sentinel/invalid)
 *   @2   1    u8     is_sunset               (1 = sunset, blocks deposits/orders/matches)
 *   @3   1    u8     kind                    (SubVaultKind: 0 = Pool, 1 = Private)
 *   @4   2    u16    spread_bps              (quoted over the market's live marginfi APR)
 *   @6   2    [u8]   _pad0
 *   @8   32   Pubkey curator
 *   @40  2    u16    max_ltv_bps             (origination LTV cap; always explicit in v1)
 *   @42  2    u16    liquidation_ltv_bps     (liquidation trigger; ≥ max_ltv_bps + MIN_LIQ_GAP_BPS)
 *   @44  4    u32    max_term_seconds
 *   @48  2    u16    curator_fee_bps         (per-sub-vault; 0 for Private)
 *   @50  2    u16    open_orders_count       (resting asks across all markets)
 *   @52  4    u32    open_loans_count        (open loans across all markets)
 *   @56  8    [u8]   _pad2
 *   @64  16   u128   total_shares
 *   @80  8    u64    total_assets_atoms
 *   @88  8    u64    total_principal_atoms
 *   @96  8    u64    deployed_principal_atoms
 *   @104 8    u64    encumbered_in_orders_atoms
 *   @112 16   u128   total_weighted_rate_bps  (gross)
 *   @128 8    u64    accumulated_curator_fee_atoms
 *   @136 8    i64    last_accrue_unix
 *   @144 16   u128   cumulative_supply_yield_index_scaled
 *   @160 16   u128   cumulative_delta_yield_index_scaled
 *   @176 16   u128   last_supply_share_value_fp48
 *   @192 32   Pubkey pending_curator
 *   @224 16   u128   total_weighted_net_rate_bps  (NET — depositors' slice)
 *   @240 8    u64    pending_claim_atoms     (retired atoms sitting in the per-market
 *                                              lender_marginfi_account, awaiting
 *                                              claim_repayment_for_sub_vault sweep)
 */
export interface SubVault {
  subVaultId: number;
  /** 1 = sub-vault sunset; deposits / new orders / order updates / matches reject. 0 = live. */
  isSunset: number;
  /** `SubVaultKind`: 0 = Pool (curator-run, pooled), 1 = Private (single-owner). */
  kind: SubVaultKind;
  /** Quoted spread over the market's live marginfi lending APR, in bps. */
  spreadBps: number;
  /** Curator pubkey; signs curator-only instructions. For Private sub-vaults this is the owner. */
  curator: PublicKey;
  /** Origination LTV cap in bps (lender-side policy, read live at match time). Always explicit in v1. */
  maxLtvBps: number;
  /** Liquidation trigger in bps; enforced ≥ `maxLtvBps + MIN_LIQ_GAP_BPS`, stamped onto loans at match. */
  liquidationLtvBps: number;
  maxTermSeconds: number;
  /** Curator management fee in bps (≤ MAX_CURATOR_FEE_BPS); 0 for Private. Snapshotted at match. */
  curatorFeeBps: number;
  /** Resting asks across all markets; inc on place, dec on cancel/admin-cancel. */
  openOrdersCount: number;
  /** Open loans (queued matches + promoted, all markets); inc at fill, dec at full close. */
  openLoansCount: number;
  totalShares: bigint;
  totalAssetsAtoms: bigint;
  totalPrincipalAtoms: bigint;
  deployedPrincipalAtoms: bigint;
  encumberedInOrdersAtoms: bigint;
  totalWeightedRateBps: bigint;
  accumulatedCuratorFeeAtoms: bigint;
  lastAccrueUnix: bigint;
  cumulativeSupplyYieldIndexScaled: bigint;
  cumulativeDeltaYieldIndexScaled: bigint;
  lastSupplyShareValueFp48: bigint;
  pendingCurator: PublicKey;
  totalWeightedNetRateBps: bigint;
  /**
   * Atoms retired by repay / liquidation / settle-matured but not yet
   * swept by `claim_repayment_for_sub_vault`. Physically held in the
   * per-market `lender_marginfi_account`, NOT the vault's own
   * integration account — excluded from the idle MTM calc.
   */
  pendingClaimAtoms: bigint;
}

export function decodeSubVault(payload: DataView): SubVault {
  return {
    subVaultId: readU16(payload, 0),
    isSunset: readU8(payload, 2),
    kind: readU8(payload, 3) as SubVaultKind,
    spreadBps: readU16(payload, 4),
    curator: readPubkey(payload, 8),
    maxLtvBps: readU16(payload, 40),
    liquidationLtvBps: readU16(payload, 42),
    maxTermSeconds: readU32(payload, 44),
    curatorFeeBps: readU16(payload, 48),
    openOrdersCount: readU16(payload, 50),
    openLoansCount: readU32(payload, 52),
    totalShares: readU128(payload, 64),
    totalAssetsAtoms: readU64(payload, 80),
    totalPrincipalAtoms: readU64(payload, 88),
    deployedPrincipalAtoms: readU64(payload, 96),
    encumberedInOrdersAtoms: readU64(payload, 104),
    totalWeightedRateBps: readU128(payload, 112),
    accumulatedCuratorFeeAtoms: readU64(payload, 128),
    lastAccrueUnix: readI64(payload, 136),
    cumulativeSupplyYieldIndexScaled: readU128(payload, 144),
    cumulativeDeltaYieldIndexScaled: readU128(payload, 160),
    lastSupplyShareValueFp48: readU128(payload, 176),
    pendingCurator: readPubkey(payload, 192),
    totalWeightedNetRateBps: readU128(payload, 224),
    pendingClaimAtoms: readU64(payload, 240),
  };
}

/* ── SubVaultDepositorSeat ────────────────────────────────── */

/**
 * Layout:
 *   @0   32   Pubkey owner
 *   @32  2    u16    sub_vault_id
 *   @34  14   [u8]   _pad0
 *   @48  16   u128   shares
 *   @64  16   u128   snapshot_supply_yield_index_scaled
 *   @80  16   u128   snapshot_delta_yield_index_scaled
 *   @96  8    i64    last_updated_unix
 */
export interface SubVaultDepositorSeat {
  owner: PublicKey;
  subVaultId: number;
  shares: bigint;
  snapshotSupplyYieldIndexScaled: bigint;
  snapshotDeltaYieldIndexScaled: bigint;
  lastUpdatedUnix: bigint;
}

export function decodeSubVaultDepositorSeat(payload: DataView): SubVaultDepositorSeat {
  return {
    owner: readPubkey(payload, 0),
    subVaultId: readU16(payload, 32),
    shares: readU128(payload, 48),
    snapshotSupplyYieldIndexScaled: readU128(payload, 64),
    snapshotDeltaYieldIndexScaled: readU128(payload, 80),
    lastUpdatedUnix: readI64(payload, 96),
  };
}

/* ── SubVaultOrderRef ─────────────────────────────────────── */

/**
 * Layout:
 *   @0   32   Pubkey market
 *   @32  2    u16    sub_vault_id
 *   @34  1    u8     side       (0 = Bid, 1 = Ask — always Ask for vault orders)
 *   @35  1    u8     _pad0
 *   @36  2    u16    rate_bps
 *   @38  2    u8[2]  _pad1
 *   @40  4    u32    term_seconds
 *   @44  4    u8[4]  _pad2
 *   @48  8    u64    order_sequence_in_market
 *   @56  8    i64    placed_at_unix
 */
export interface SubVaultOrderRef {
  market: PublicKey;
  subVaultId: number;
  side: number;
  rateBps: number;
  termSeconds: number;
  orderSequenceInMarket: bigint;
  placedAtUnix: bigint;
}

export function decodeSubVaultOrderRef(payload: DataView): SubVaultOrderRef {
  return {
    market: readPubkey(payload, 0),
    subVaultId: readU16(payload, 32),
    side: readU8(payload, 34),
    rateBps: readU16(payload, 36),
    termSeconds: readU32(payload, 40),
    orderSequenceInMarket: readU64(payload, 48),
    placedAtUnix: readI64(payload, 56),
  };
}
