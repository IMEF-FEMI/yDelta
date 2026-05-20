/**
 * UI-readable snapshot of every `RiskProfile` field — atoms → UI floats and
 * bps → percent. Mirrors the structure of `accounts/riskProfile.ts`'s
 * `RiskProfile` exactly, with each numeric field re-stated in the unit a UI
 * actually wants.
 */
import { PublicKey } from '@solana/web3.js';

import { atomsToUi, bpsToPercent, fp48ToFloat } from './conversions.js';
import type { RiskProfile } from './accounts/riskProfile.js';

export interface RiskProfileUi {
  profileId: number;
  curator: PublicKey;
  pendingCurator: PublicKey;
  /** Policy. */
  maxLtvBps: number;
  maxLtvPercent: number;
  maxTermSeconds: number;
  maxTermDays: number;
  /** Capital pools. */
  totalPrincipalAtoms: bigint;
  totalPrincipalUi: number;
  totalAssetsAtoms: bigint;
  totalAssetsUi: number;
  deployedPrincipalAtoms: bigint;
  deployedPrincipalUi: number;
  encumberedInOrdersAtoms: bigint;
  encumberedInOrdersUi: number;
  idleAtoms: bigint;
  idleUi: number;
  /** Shares. */
  totalShares: bigint;
  /**
   * NAV per share: `total_assets_atoms / total_shares` (atoms-per-share, NOT
   * fp48-scaled). `0` when total_shares is zero.
   */
  navPerShare: number;
  /** Weighted-rate aggregates. */
  totalWeightedRateBps: bigint;
  totalWeightedNetRateBps: bigint;
  /** Average gross / net lender rate, as bps, derived from deployed_principal_atoms. */
  averageGrossLenderRateBps: number;
  averageNetLenderRateBps: number;
  /** Accruals. */
  accumulatedCuratorFeeAtoms: bigint;
  accumulatedCuratorFeeUi: number;
  cumulativeSupplyYieldIndex: number;
  cumulativeDeltaYieldIndex: number;
  lastSupplyShareValue: number;
  lastAccrueUnix: bigint;
}

export function riskProfileUi(profile: RiskProfile, decimals: number): RiskProfileUi {
  const idle =
    profile.totalPrincipalAtoms >= profile.deployedPrincipalAtoms
      ? profile.totalPrincipalAtoms - profile.deployedPrincipalAtoms
      : 0n;

  const navPerShare =
    profile.totalShares === 0n
      ? 0
      : atomsToUi(
          (profile.totalAssetsAtoms * 10n ** BigInt(decimals)) / profile.totalShares,
          decimals,
        );

  const avgGross =
    profile.deployedPrincipalAtoms === 0n
      ? 0
      : Number(profile.totalWeightedRateBps / profile.deployedPrincipalAtoms);
  const avgNet =
    profile.deployedPrincipalAtoms === 0n
      ? 0
      : Number(profile.totalWeightedNetRateBps / profile.deployedPrincipalAtoms);

  return {
    profileId: profile.profileId,
    curator: profile.curator,
    pendingCurator: profile.pendingCurator,
    maxLtvBps: profile.maxLtvBps,
    maxLtvPercent: bpsToPercent(profile.maxLtvBps),
    maxTermSeconds: profile.maxTermSeconds,
    maxTermDays: profile.maxTermSeconds / 86_400,
    totalPrincipalAtoms: profile.totalPrincipalAtoms,
    totalPrincipalUi: atomsToUi(profile.totalPrincipalAtoms, decimals),
    totalAssetsAtoms: profile.totalAssetsAtoms,
    totalAssetsUi: atomsToUi(profile.totalAssetsAtoms, decimals),
    deployedPrincipalAtoms: profile.deployedPrincipalAtoms,
    deployedPrincipalUi: atomsToUi(profile.deployedPrincipalAtoms, decimals),
    encumberedInOrdersAtoms: profile.encumberedInOrdersAtoms,
    encumberedInOrdersUi: atomsToUi(profile.encumberedInOrdersAtoms, decimals),
    idleAtoms: idle,
    idleUi: atomsToUi(idle, decimals),
    totalShares: profile.totalShares,
    navPerShare,
    totalWeightedRateBps: profile.totalWeightedRateBps,
    totalWeightedNetRateBps: profile.totalWeightedNetRateBps,
    averageGrossLenderRateBps: avgGross,
    averageNetLenderRateBps: avgNet,
    accumulatedCuratorFeeAtoms: profile.accumulatedCuratorFeeAtoms,
    accumulatedCuratorFeeUi: atomsToUi(profile.accumulatedCuratorFeeAtoms, decimals),
    cumulativeSupplyYieldIndex: fp48ToFloat(profile.cumulativeSupplyYieldIndexScaled),
    cumulativeDeltaYieldIndex: fp48ToFloat(profile.cumulativeDeltaYieldIndexScaled),
    lastSupplyShareValue: fp48ToFloat(profile.lastSupplyShareValueFp48),
    lastAccrueUnix: profile.lastAccrueUnix,
  };
}
