/**
 * Vault UI helpers — atoms-and-UI-units snapshots derived from a decoded
 * `GlobalVault`. Mirrors the lifecycle invariants in section 3 of the README:
 * a profile's principal decomposes into idle + deployed + encumbered, all of
 * which physically sit on marginfi and earn supply yield.
 */
import { atomsToUi } from './conversions.js';
import type { RiskProfile } from './accounts/riskProfile.js';
import type { GlobalVault } from './accounts/vault.js';

export interface ProfileBalances {
  profileId: number;
  /** Total principal contributed by depositors. */
  totalPrincipalAtoms: bigint;
  /** Profile NAV — total_assets_atoms (principal + accrued yield). */
  totalAssetsAtoms: bigint;
  /** Currently lent out across all loans. */
  deployedPrincipalAtoms: bigint;
  /** Earmarked by resting orders (bookkeeping only — atoms still on marginfi). */
  encumberedInOrdersAtoms: bigint;
  /** `total_principal − deployed`. Withdraw-eligible. */
  idleAtoms: bigint;
  totalShares: bigint;
  accumulatedCuratorFeeAtoms: bigint;
}

export interface ProfileBalancesUi extends ProfileBalances {
  totalPrincipalUi: number;
  totalAssetsUi: number;
  deployedPrincipalUi: number;
  encumberedInOrdersUi: number;
  idleUi: number;
  accumulatedCuratorFeeUi: number;
}

/** Compute the idle / deployed / encumbered decomposition for one profile. */
export function profileBalances(profile: RiskProfile): ProfileBalances {
  const idle =
    profile.totalPrincipalAtoms >= profile.deployedPrincipalAtoms
      ? profile.totalPrincipalAtoms - profile.deployedPrincipalAtoms
      : 0n;
  return {
    profileId: profile.profileId,
    totalPrincipalAtoms: profile.totalPrincipalAtoms,
    totalAssetsAtoms: profile.totalAssetsAtoms,
    deployedPrincipalAtoms: profile.deployedPrincipalAtoms,
    encumberedInOrdersAtoms: profile.encumberedInOrdersAtoms,
    idleAtoms: idle,
    totalShares: profile.totalShares,
    accumulatedCuratorFeeAtoms: profile.accumulatedCuratorFeeAtoms,
  };
}

/** Same as `profileBalances` plus UI-float projections at the given mint decimals. */
export function profileBalancesUi(profile: RiskProfile, decimals: number): ProfileBalancesUi {
  const base = profileBalances(profile);
  return {
    ...base,
    totalPrincipalUi: atomsToUi(base.totalPrincipalAtoms, decimals),
    totalAssetsUi: atomsToUi(base.totalAssetsAtoms, decimals),
    deployedPrincipalUi: atomsToUi(base.deployedPrincipalAtoms, decimals),
    encumberedInOrdersUi: atomsToUi(base.encumberedInOrdersAtoms, decimals),
    idleUi: atomsToUi(base.idleAtoms, decimals),
    accumulatedCuratorFeeUi: atomsToUi(base.accumulatedCuratorFeeAtoms, decimals),
  };
}

/**
 * Sum aggregates across every profile in the vault. The on-chain header
 * intentionally carries NO mirrored running sums — see the doc-comment on
 * `GlobalVaultFixed` — so this is the canonical way to compute a vault-wide
 * total.
 */
export interface VaultTotals {
  totalPrincipalAtoms: bigint;
  totalAssetsAtoms: bigint;
  totalDeployedAtoms: bigint;
  totalEncumberedAtoms: bigint;
  totalIdleAtoms: bigint;
  totalShares: bigint;
  profileCount: number;
}

export function vaultTotals(vault: GlobalVault): VaultTotals {
  let totalPrincipalAtoms = 0n;
  let totalAssetsAtoms = 0n;
  let totalDeployedAtoms = 0n;
  let totalEncumberedAtoms = 0n;
  let totalIdleAtoms = 0n;
  let totalShares = 0n;
  for (const { profile } of vault.riskProfiles) {
    const b = profileBalances(profile);
    totalPrincipalAtoms += b.totalPrincipalAtoms;
    totalAssetsAtoms += b.totalAssetsAtoms;
    totalDeployedAtoms += b.deployedPrincipalAtoms;
    totalEncumberedAtoms += b.encumberedInOrdersAtoms;
    totalIdleAtoms += b.idleAtoms;
    totalShares += b.totalShares;
  }
  return {
    totalPrincipalAtoms,
    totalAssetsAtoms,
    totalDeployedAtoms,
    totalEncumberedAtoms,
    totalIdleAtoms,
    totalShares,
    profileCount: vault.riskProfiles.length,
  };
}

export interface VaultTotalsUi extends VaultTotals {
  totalPrincipalUi: number;
  totalAssetsUi: number;
  totalDeployedUi: number;
  totalEncumberedUi: number;
  totalIdleUi: number;
}

export function vaultTotalsUi(vault: GlobalVault, decimals: number): VaultTotalsUi {
  const t = vaultTotals(vault);
  return {
    ...t,
    totalPrincipalUi: atomsToUi(t.totalPrincipalAtoms, decimals),
    totalAssetsUi: atomsToUi(t.totalAssetsAtoms, decimals),
    totalDeployedUi: atomsToUi(t.totalDeployedAtoms, decimals),
    totalEncumberedUi: atomsToUi(t.totalEncumberedAtoms, decimals),
    totalIdleUi: atomsToUi(t.totalIdleAtoms, decimals),
  };
}
