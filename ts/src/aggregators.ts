/**
 * Aggregators — single-shot summaries of a vault's earning posture.
 *
 * The on-chain `RiskProfile.total_weighted_net_rate_bps / Σ
 * deployed_principal_atoms` is the **authoritative** average lender rate (net
 * of curator fee). Prefer it over walking per-market asks. This module
 * exposes the authoritative view + an optional per-market traversal for
 * explanatory breakdowns.
 */
import type { RiskProfile } from './accounts/riskProfile.js';
import type { GlobalVault } from './accounts/vault.js';

/** Authoritative net average rate (bps) across a single profile's open loans. */
export function profileAverageNetRateBps(profile: RiskProfile): number {
  if (profile.deployedPrincipalAtoms === 0n) return 0;
  return Number(profile.totalWeightedNetRateBps / profile.deployedPrincipalAtoms);
}

/** Authoritative gross average rate (bps) — pre-curator-fee. */
export function profileAverageGrossRateBps(profile: RiskProfile): number {
  if (profile.deployedPrincipalAtoms === 0n) return 0;
  return Number(profile.totalWeightedRateBps / profile.deployedPrincipalAtoms);
}

/** Weighted-average net rate across an entire vault, weighted by deployed principal. */
export function vaultAverageNetRateBps(vault: GlobalVault): number {
  let weighted = 0n;
  let totalDeployed = 0n;
  for (const { profile } of vault.riskProfiles) {
    weighted += profile.totalWeightedNetRateBps;
    totalDeployed += profile.deployedPrincipalAtoms;
  }
  if (totalDeployed === 0n) return 0;
  return Number(weighted / totalDeployed);
}

/** Same shape as `vaultAverageNetRateBps` but for the gross aggregate. */
export function vaultAverageGrossRateBps(vault: GlobalVault): number {
  let weighted = 0n;
  let totalDeployed = 0n;
  for (const { profile } of vault.riskProfiles) {
    weighted += profile.totalWeightedRateBps;
    totalDeployed += profile.deployedPrincipalAtoms;
  }
  if (totalDeployed === 0n) return 0;
  return Number(weighted / totalDeployed);
}

/** Per-profile breakdown — useful for UIs listing strategies side-by-side. */
export interface ProfileRateBreakdown {
  profileId: number;
  deployedPrincipalAtoms: bigint;
  averageGrossRateBps: number;
  averageNetRateBps: number;
}

export function vaultRateBreakdown(vault: GlobalVault): ProfileRateBreakdown[] {
  return vault.riskProfiles.map(({ profile }) => ({
    profileId: profile.profileId,
    deployedPrincipalAtoms: profile.deployedPrincipalAtoms,
    averageGrossRateBps: profileAverageGrossRateBps(profile),
    averageNetRateBps: profileAverageNetRateBps(profile),
  }));
}
