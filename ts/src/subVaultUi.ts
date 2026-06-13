/**
 * UI-readable snapshot of every `SubVault` field — atoms → UI floats and
 * bps → percent. Mirrors the structure of `accounts/subVault.ts`'s
 * `SubVault` exactly, with each numeric field re-stated in the unit a UI
 * actually wants.
 */
import { PublicKey } from '@solana/web3.js';

import { atomsToUi, bpsToPercent, fp48ToFloat } from './conversions.js';
import type { SubVault } from './accounts/subVault.js';

export interface SubVaultUi {
  subVaultId: number;
  curator: PublicKey;
  pendingCurator: PublicKey;
  /** Policy. */
  maxLtvBps: number;
  /** `maxLtvBps` rendered as a percent. Always an explicit cap in v1. */
  maxLtvPercent: number;
  /** Liquidation trigger in bps; stamped onto loans at match. */
  liquidationLtvBps: number;
  /** `liquidationLtvBps` rendered as a percent. */
  liquidationLtvPercent: number;
  maxTermSeconds: number;
  maxTermDays: number;
  /**
   * `true` when `subVault.is_sunset == 1`. Sunset sub-vaults reject deposits,
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

export function subVaultUi(subVault: SubVault, decimals: number): SubVaultUi {
  const idle =
    subVault.totalPrincipalAtoms >= subVault.deployedPrincipalAtoms
      ? subVault.totalPrincipalAtoms - subVault.deployedPrincipalAtoms
      : 0n;

  const navPerShare =
    subVault.totalShares === 0n
      ? 0
      : atomsToUi(
          (subVault.totalAssetsAtoms * 10n ** BigInt(decimals)) / subVault.totalShares,
          decimals,
        );

  const avgGross =
    subVault.deployedPrincipalAtoms === 0n
      ? 0
      : Number(subVault.totalWeightedRateBps / subVault.deployedPrincipalAtoms);
  const avgNet =
    subVault.deployedPrincipalAtoms === 0n
      ? 0
      : Number(subVault.totalWeightedNetRateBps / subVault.deployedPrincipalAtoms);

  return {
    subVaultId: subVault.subVaultId,
    curator: subVault.curator,
    pendingCurator: subVault.pendingCurator,
    maxLtvBps: subVault.maxLtvBps,
    maxLtvPercent: bpsToPercent(subVault.maxLtvBps),
    liquidationLtvBps: subVault.liquidationLtvBps,
    liquidationLtvPercent: bpsToPercent(subVault.liquidationLtvBps),
    maxTermSeconds: subVault.maxTermSeconds,
    maxTermDays: subVault.maxTermSeconds / 86_400,
    isSunset: subVault.isSunset !== 0,
    totalPrincipalAtoms: subVault.totalPrincipalAtoms,
    totalPrincipalUi: atomsToUi(subVault.totalPrincipalAtoms, decimals),
    totalAssetsAtoms: subVault.totalAssetsAtoms,
    totalAssetsUi: atomsToUi(subVault.totalAssetsAtoms, decimals),
    deployedPrincipalAtoms: subVault.deployedPrincipalAtoms,
    deployedPrincipalUi: atomsToUi(subVault.deployedPrincipalAtoms, decimals),
    encumberedInOrdersAtoms: subVault.encumberedInOrdersAtoms,
    encumberedInOrdersUi: atomsToUi(subVault.encumberedInOrdersAtoms, decimals),
    idleAtoms: idle,
    idleUi: atomsToUi(idle, decimals),
    totalShares: subVault.totalShares,
    navPerShare,
    totalWeightedRateBps: subVault.totalWeightedRateBps,
    totalWeightedNetRateBps: subVault.totalWeightedNetRateBps,
    averageGrossLenderRateBps: avgGross,
    averageNetLenderRateBps: avgNet,
    accumulatedCuratorFeeAtoms: subVault.accumulatedCuratorFeeAtoms,
    accumulatedCuratorFeeUi: atomsToUi(subVault.accumulatedCuratorFeeAtoms, decimals),
    cumulativeSupplyYieldIndex: fp48ToFloat(subVault.cumulativeSupplyYieldIndexScaled),
    cumulativeDeltaYieldIndex: fp48ToFloat(subVault.cumulativeDeltaYieldIndexScaled),
    lastSupplyShareValue: fp48ToFloat(subVault.lastSupplyShareValueFp48),
    lastAccrueUnix: subVault.lastAccrueUnix,
  };
}
