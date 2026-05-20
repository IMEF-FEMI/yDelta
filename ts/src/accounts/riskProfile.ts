/**
 * `RiskProfile` (496 bytes) + `RiskProfileDepositorSeat` (144 bytes) +
 * `RiskProfileOrderRef` (144 bytes) — the three tree-node payloads in a
 * `GlobalVault`'s dynamic region. RiskProfile lives in the 512-byte free
 * list; the other two share the 160-byte free list.
 */
import { PublicKey } from '@solana/web3.js';

import { readI64, readPubkey, readU128, readU16, readU32, readU64, readU8 } from './_read.js';

export const RISK_PROFILE_SIZE = 496;
export const RISK_PROFILE_DEPOSITOR_SEAT_SIZE = 144;
export const RISK_PROFILE_ORDER_REF_SIZE = 144;

/* ── RiskProfile ──────────────────────────────────────────── */

/**
 * Layout (offsets from state/vault.rs):
 *   @0   1    u8     profile_id
 *   @8   32   Pubkey curator
 *   @40  2    u16    max_ltv_bps
 *   @44  4    u32    max_term_seconds
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
 */
export interface RiskProfile {
  profileId: number;
  curator: PublicKey;
  maxLtvBps: number;
  maxTermSeconds: number;
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
}

export function decodeRiskProfile(payload: DataView): RiskProfile {
  return {
    profileId: readU8(payload, 0),
    curator: readPubkey(payload, 8),
    maxLtvBps: readU16(payload, 40),
    maxTermSeconds: readU32(payload, 44),
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
  };
}

/* ── RiskProfileDepositorSeat ─────────────────────────────── */

/**
 * Layout:
 *   @0   32   Pubkey owner
 *   @32  1    u8     profile_id
 *   @48  16   u128   shares
 *   @64  16   u128   snapshot_supply_yield_index_scaled
 *   @80  16   u128   snapshot_delta_yield_index_scaled
 *   @96  8    i64    last_updated_unix
 */
export interface RiskProfileDepositorSeat {
  owner: PublicKey;
  profileId: number;
  shares: bigint;
  snapshotSupplyYieldIndexScaled: bigint;
  snapshotDeltaYieldIndexScaled: bigint;
  lastUpdatedUnix: bigint;
}

export function decodeRiskProfileDepositorSeat(payload: DataView): RiskProfileDepositorSeat {
  return {
    owner: readPubkey(payload, 0),
    profileId: readU8(payload, 32),
    shares: readU128(payload, 48),
    snapshotSupplyYieldIndexScaled: readU128(payload, 64),
    snapshotDeltaYieldIndexScaled: readU128(payload, 80),
    lastUpdatedUnix: readI64(payload, 96),
  };
}

/* ── RiskProfileOrderRef ──────────────────────────────────── */

/**
 * Layout:
 *   @0   32   Pubkey market
 *   @32  1    u8     profile_id
 *   @33  1    u8     side       (0 = Bid, 1 = Ask — always Ask for vault orders)
 *   @36  2    u16    rate_bps
 *   @40  4    u32    term_seconds
 *   @48  8    u64    order_sequence_in_market
 *   @56  8    i64    placed_at_unix
 */
export interface RiskProfileOrderRef {
  market: PublicKey;
  profileId: number;
  side: number;
  rateBps: number;
  termSeconds: number;
  orderSequenceInMarket: bigint;
  placedAtUnix: bigint;
}

export function decodeRiskProfileOrderRef(payload: DataView): RiskProfileOrderRef {
  return {
    market: readPubkey(payload, 0),
    profileId: readU8(payload, 32),
    side: readU8(payload, 33),
    rateBps: readU16(payload, 36),
    termSeconds: readU32(payload, 40),
    orderSequenceInMarket: readU64(payload, 48),
    placedAtUnix: readI64(payload, 56),
  };
}
