/**
 * Vault UI helpers — atoms-and-UI-units snapshots derived from a decoded
 * `GlobalVault`. Mirrors the lifecycle invariants in section 3 of the README:
 * a sub-vault's principal decomposes into idle + deployed + encumbered, all of
 * which physically sit on marginfi and earn supply yield.
 */
import { atomsToUi } from './conversions.js';
import type { SubVault } from './accounts/subVault.js';
import type { GlobalVault } from './accounts/vault.js';

export interface SubVaultBalances {
  subVaultId: number;
  /** Total principal contributed by depositors. */
  totalPrincipalAtoms: bigint;
  /** Sub-vault NAV — total_assets_atoms (principal + accrued yield). */
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

export interface SubVaultBalancesUi extends SubVaultBalances {
  totalPrincipalUi: number;
  totalAssetsUi: number;
  deployedPrincipalUi: number;
  encumberedInOrdersUi: number;
  idleUi: number;
  accumulatedCuratorFeeUi: number;
}

/** Compute the idle / deployed / encumbered decomposition for one sub-vault. */
export function subVaultBalances(subVault: SubVault): SubVaultBalances {
  const idle =
    subVault.totalPrincipalAtoms >= subVault.deployedPrincipalAtoms
      ? subVault.totalPrincipalAtoms - subVault.deployedPrincipalAtoms
      : 0n;
  return {
    subVaultId: subVault.subVaultId,
    totalPrincipalAtoms: subVault.totalPrincipalAtoms,
    totalAssetsAtoms: subVault.totalAssetsAtoms,
    deployedPrincipalAtoms: subVault.deployedPrincipalAtoms,
    encumberedInOrdersAtoms: subVault.encumberedInOrdersAtoms,
    idleAtoms: idle,
    totalShares: subVault.totalShares,
    accumulatedCuratorFeeAtoms: subVault.accumulatedCuratorFeeAtoms,
  };
}

/** Same as `subVaultBalances` plus UI-float projections at the given mint decimals. */
export function subVaultBalancesUi(subVault: SubVault, decimals: number): SubVaultBalancesUi {
  const base = subVaultBalances(subVault);
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
 * Sum aggregates across every sub-vault in the vault. The on-chain header
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
  subVaultCount: number;
}

export function vaultTotals(vault: GlobalVault): VaultTotals {
  let totalPrincipalAtoms = 0n;
  let totalAssetsAtoms = 0n;
  let totalDeployedAtoms = 0n;
  let totalEncumberedAtoms = 0n;
  let totalIdleAtoms = 0n;
  let totalShares = 0n;
  for (const { subVault } of vault.subVaults) {
    const b = subVaultBalances(subVault);
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
    subVaultCount: vault.subVaults.length,
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
