/**
 * Aggregators — single-shot summaries of a vault's earning posture.
 *
 * The on-chain `SubVault.total_weighted_net_rate_bps / Σ
 * deployed_principal_atoms` is the **authoritative** average lender rate (net
 * of curator fee). Prefer it over walking per-market asks. This module
 * exposes the authoritative view + an optional per-market traversal for
 * explanatory breakdowns.
 */
import type { SubVault } from './accounts/subVault.js';
import type { GlobalVault } from './accounts/vault.js';

/** Authoritative net average rate (bps) across a single sub-vault's open loans. */
export function subVaultAverageNetRateBps(subVault: SubVault): number {
  if (subVault.deployedPrincipalAtoms === 0n) return 0;
  return Number(subVault.totalWeightedNetRateBps / subVault.deployedPrincipalAtoms);
}

/** Authoritative gross average rate (bps) — pre-curator-fee. */
export function subVaultAverageGrossRateBps(subVault: SubVault): number {
  if (subVault.deployedPrincipalAtoms === 0n) return 0;
  return Number(subVault.totalWeightedRateBps / subVault.deployedPrincipalAtoms);
}

/** Weighted-average net rate across an entire vault, weighted by deployed principal. */
export function vaultAverageNetRateBps(vault: GlobalVault): number {
  let weighted = 0n;
  let totalDeployed = 0n;
  for (const { subVault } of vault.subVaults) {
    weighted += subVault.totalWeightedNetRateBps;
    totalDeployed += subVault.deployedPrincipalAtoms;
  }
  if (totalDeployed === 0n) return 0;
  return Number(weighted / totalDeployed);
}

/** Same shape as `vaultAverageNetRateBps` but for the gross aggregate. */
export function vaultAverageGrossRateBps(vault: GlobalVault): number {
  let weighted = 0n;
  let totalDeployed = 0n;
  for (const { subVault } of vault.subVaults) {
    weighted += subVault.totalWeightedRateBps;
    totalDeployed += subVault.deployedPrincipalAtoms;
  }
  if (totalDeployed === 0n) return 0;
  return Number(weighted / totalDeployed);
}

/** Per-sub-vault breakdown — useful for UIs listing strategies side-by-side. */
export interface SubVaultRateBreakdown {
  subVaultId: number;
  deployedPrincipalAtoms: bigint;
  averageGrossRateBps: number;
  averageNetRateBps: number;
}

export function vaultRateBreakdown(vault: GlobalVault): SubVaultRateBreakdown[] {
  return vault.subVaults.map(({ subVault }) => ({
    subVaultId: subVault.subVaultId,
    deployedPrincipalAtoms: subVault.deployedPrincipalAtoms,
    averageGrossRateBps: subVaultAverageGrossRateBps(subVault),
    averageNetRateBps: subVaultAverageNetRateBps(subVault),
  }));
}
