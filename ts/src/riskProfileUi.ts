/**
 * UI-readable snapshot of every `RiskProfile` field — atoms → UI floats and
 * bps → percent. Mirrors the structure of `accounts/riskProfile.ts`'s
 * `RiskProfile` exactly, with each numeric field re-stated in the unit a UI
 * actually wants.
 */
import { PublicKey } from '@solana/web3.js';

import { atomsToUi, bpsToPercent, fp48ToFloat } from './conversions.js';
import { LTV_AUTO_FROM_MARGINFI } from './constants.js';
import type { RiskProfile } from './accounts/riskProfile.js';

export interface RiskProfileUi {
  profileId: number;
  curator: PublicKey;
  pendingCurator: PublicKey;
  /** Policy. */
  maxLtvBps: number;
  /**
   * `maxLtvBps` rendered as a percent. NOTE: when `maxLtvIsAuto` is true,
   * the underlying stored value is the `LTV_AUTO_FROM_MARGINFI` sentinel
   * (65535) which renders as 655.35% — UIs should branch on
   * `maxLtvIsAuto` and show "Auto" instead of this number, or call
   * `effectiveMaxLtvBpsForProfile(...)` to get the resolved cap.
   */
  maxLtvPercent: number;
  /**
   * `true` when the profile was created with `max_ltv_bps = None`. The
   * match engine resolves the cap from the live marginfi bank weights
   * at fill time as `floor(coll_w/debt_w × 10_000) − LTV_AUTO_BUFFER_BPS`.
   */
  maxLtvIsAuto: boolean;
  maxTermSeconds: number;
  maxTermDays: number;
  /**
   * `true` when `profile.is_sunset == 1`. Sunset profiles reject deposits,
   * new orders, order updates, and matches; withdrawals, cancels, fee
   * claims, repay, settle, and liquidate stay open.
   */
  isSunset: boolean;
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
    maxLtvIsAuto: profile.maxLtvBps === LTV_AUTO_FROM_MARGINFI,
    maxTermSeconds: profile.maxTermSeconds,
    maxTermDays: profile.maxTermSeconds / 86_400,
    isSunset: profile.isSunset !== 0,
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
