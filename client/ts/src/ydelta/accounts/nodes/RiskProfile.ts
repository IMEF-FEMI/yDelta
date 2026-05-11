import { PublicKey } from '@solana/web3.js';
import BN from 'bn.js';

export const RISK_PROFILE_SIZE = 496;
export const RISK_PROFILE_ACTIVE_MARKETS_MAX = 8;

export type RiskProfile = {
  profileId: number;
  curator: PublicKey;
  /** bps */
  maxLtvBps: number;
  /** seconds */
  maxTermSeconds: number;
  allowedMarketCount: number;
  allowedMarketMax: number;
  /** fp48 share units */
  totalShares: BN;
  /** atoms */
  totalAssetsAtoms: BN;
  /** atoms */
  totalPrincipalAtoms: BN;
  /** atoms */
  deployedPrincipalAtoms: BN;
  /** atoms */
  encumberedInOrdersAtoms: BN;
  /** Σ(principal_atoms × rate_bps), u128 */
  totalWeightedRateBps: BN;
  /** atoms */
  accumulatedCuratorFeeAtoms: BN;
  /** unix seconds */
  lastAccrueUnix: BN;
  /** Aave-style cumulative yield index, × 2^48 */
  cumulativeSupplyYieldIndexScaled: BN;
  /** Aave-style cumulative yield index, × 2^48 */
  cumulativeDeltaYieldIndexScaled: BN;
  /** fp48 (raw I80F48 mantissa as u128) */
  lastSupplyShareValueFp48: BN;
  pendingCurator: PublicKey;
  activeMarkets: PublicKey[];
};

function readU128(buf: Buffer, offset: number): BN {
  return new BN(buf.subarray(offset, offset + 16), 'le');
}

function readI64(buf: Buffer, offset: number): BN {
  const u = new BN(buf.subarray(offset, offset + 8), 'le');
  const TWO_64 = new BN(1).shln(64);
  return u.testn(63) ? u.sub(TWO_64) : u;
}

export function decodeRiskProfile(buf: Buffer): RiskProfile {
  const profileId = buf.readUInt8(0);
  const curator = new PublicKey(buf.subarray(8, 40));
  const maxLtvBps = buf.readUInt16LE(40);
  const maxTermSeconds = buf.readUInt32LE(44);
  const allowedMarketCount = buf.readUInt8(48);
  const allowedMarketMax = buf.readUInt8(49);
  const totalShares = readU128(buf, 64);
  const totalAssetsAtoms = new BN(buf.subarray(80, 88), 'le');
  const totalPrincipalAtoms = new BN(buf.subarray(88, 96), 'le');
  const deployedPrincipalAtoms = new BN(buf.subarray(96, 104), 'le');
  const encumberedInOrdersAtoms = new BN(buf.subarray(104, 112), 'le');
  const totalWeightedRateBps = readU128(buf, 112);
  const accumulatedCuratorFeeAtoms = new BN(buf.subarray(128, 136), 'le');
  const lastAccrueUnix = readI64(buf, 136);
  const cumulativeSupplyYieldIndexScaled = readU128(buf, 144);
  const cumulativeDeltaYieldIndexScaled = readU128(buf, 160);
  const lastSupplyShareValueFp48 = readU128(buf, 176);
  const pendingCurator = new PublicKey(buf.subarray(192, 224));
  const activeMarkets: PublicKey[] = [];
  for (let i = 0; i < allowedMarketCount && i < RISK_PROFILE_ACTIVE_MARKETS_MAX; i++) {
    activeMarkets.push(new PublicKey(buf.subarray(224 + i * 32, 224 + (i + 1) * 32)));
  }
  return {
    profileId,
    curator,
    maxLtvBps,
    maxTermSeconds,
    allowedMarketCount,
    allowedMarketMax,
    totalShares,
    totalAssetsAtoms,
    totalPrincipalAtoms,
    deployedPrincipalAtoms,
    encumberedInOrdersAtoms,
    totalWeightedRateBps,
    accumulatedCuratorFeeAtoms,
    lastAccrueUnix,
    cumulativeSupplyYieldIndexScaled,
    cumulativeDeltaYieldIndexScaled,
    lastSupplyShareValueFp48,
    pendingCurator,
    activeMarkets,
  };
}

/** Principal still available to back new orders. Mirrors the on-chain
 *  matching gate (`programs/ydelta/src/state/market_helpers.rs` and
 *  `vault.rs`): `idle = total_principal_atoms - deployed - encumbered`.
 *
 *  Note: this is NOT the redeemable-by-LP amount — it excludes accrued
 *  supply + delta yield, which is exactly the on-chain semantics for
 *  origination/encumbrance checks. For "how much can be withdrawn", see
 *  `withdrawableAtoms`. */
export function idleAtoms(profile: RiskProfile): BN {
  const reserved = profile.deployedPrincipalAtoms.add(profile.encumberedInOrdersAtoms);
  const idle = profile.totalPrincipalAtoms.sub(reserved);
  return idle.isNeg() ? new BN(0) : idle;
}

/** Atoms withdrawable by depositors (assets-side accounting; includes
 *  accrued yield). Use for UI redemption previews, NOT for sizing new
 *  orders — origination is gated on `idleAtoms`. */
export function withdrawableAtoms(profile: RiskProfile): BN {
  const reserved = profile.deployedPrincipalAtoms.add(profile.encumberedInOrdersAtoms);
  const idle = profile.totalAssetsAtoms.sub(reserved);
  return idle.isNeg() ? new BN(0) : idle;
}

/** Weighted-average lender rate across open loans. */
export function weightedAvgRateBps(profile: RiskProfile): number {
  if (profile.deployedPrincipalAtoms.isZero()) return 0;
  // total_weighted_rate_bps = Σ(principal × rate); divide by deployed principal.
  return profile.totalWeightedRateBps
    .div(profile.deployedPrincipalAtoms)
    .toNumber();
}
