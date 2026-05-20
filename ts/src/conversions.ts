import BN from 'bn.js';

import { BPS_PER_UNIT, FP48_ONE, FP48_SHIFT } from './constants.js';

/* ─────────────────────────── atoms ↔ UI ─────────────────────────── */

/** Convert raw on-chain atoms (smallest mint units) to a UI float. */
export function atomsToUi(atoms: bigint | BN | number, decimals: number): number {
  const big = toBigInt(atoms);
  // Split int and fractional parts to avoid lossy Number(big) on > 2^53.
  const scale = 10n ** BigInt(decimals);
  const whole = big / scale;
  const frac = big % scale;
  return Number(whole) + Number(frac) / Number(scale);
}

/** Convert a UI float to raw on-chain atoms. Rounds DOWN (floor). */
export function uiToAtoms(ui: number, decimals: number): bigint {
  if (!Number.isFinite(ui) || ui < 0) {
    throw new RangeError(`uiToAtoms: invalid ui amount ${ui}`);
  }
  const scale = 10 ** decimals;
  // Stringify through fixed-point to avoid float imprecision past ~15 sig figs.
  const scaled = Math.floor(ui * scale);
  return BigInt(scaled);
}

/* ─────────────────────────── fp48 ↔ float ───────────────────────── */

/** Convert an on-chain fp48 (`value << 48`) `u128` to a float. */
export function fp48ToFloat(fp48: bigint | BN): number {
  const big = toBigInt(fp48);
  const whole = big >> FP48_SHIFT;
  const fracRaw = big & (FP48_ONE - 1n);
  return Number(whole) + Number(fracRaw) / Number(FP48_ONE);
}

/** Convert a float (USD price, weight, share value, …) to fp48 `u128`. */
export function floatToFp48(value: number): bigint {
  if (!Number.isFinite(value) || value < 0) {
    throw new RangeError(`floatToFp48: invalid value ${value}`);
  }
  // Use BigInt math via the fractional split to avoid Number precision loss.
  const whole = BigInt(Math.floor(value));
  const frac = value - Math.floor(value);
  const fracFp = BigInt(Math.floor(frac * Number(FP48_ONE)));
  return (whole << FP48_SHIFT) + fracFp;
}

/* ──────────────────────── shares ↔ atoms ────────────────────────── */

/**
 * Convert marginfi (or vault) shares to atoms at a given share-value snapshot.
 * `shares × share_value_fp48 >> 48`. Mirrors `atoms_to_shares_at_snapshot`'s
 * inverse used throughout the program.
 */
export function sharesToAtoms(shares: bigint | BN, shareValueFp48: bigint | BN): bigint {
  const s = toBigInt(shares);
  const v = toBigInt(shareValueFp48);
  return (s * v) >> FP48_SHIFT;
}

/** Atoms → shares at a snapshot, mirroring `atoms_to_shares_at_snapshot`. */
export function atomsToShares(atoms: bigint | BN, shareValueFp48: bigint | BN): bigint {
  const a = toBigInt(atoms);
  const v = toBigInt(shareValueFp48);
  if (v === 0n) throw new RangeError('atomsToShares: zero share value');
  return (a << FP48_SHIFT) / v;
}

/* ───────────────────────── bps ↔ percent ────────────────────────── */

/** Convert basis points to a fractional rate (e.g. 500 → 0.05). */
export function bpsToFraction(bps: number): number {
  return bps / Number(BPS_PER_UNIT);
}

/** Convert basis points to a UI percent (e.g. 500 → 5.0). */
export function bpsToPercent(bps: number): number {
  return bps / 100;
}

/** Convert a UI percent back to bps (e.g. 5.0 → 500). Rounds to nearest. */
export function percentToBps(percent: number): number {
  return Math.round(percent * 100);
}

/* ─────────────────────── vault-share ↔ atoms ────────────────────── */

/**
 * Convert a depositor's vault-share balance to its atom value.
 * `atoms = shares × total_assets_atoms / total_shares`. Rounds DOWN — matches
 * the on-chain redemption math in `global_vault_withdraw`.
 */
export function vaultSharesToAtoms(
  shares: bigint | BN,
  totalShares: bigint | BN,
  totalAssetsAtoms: bigint | BN,
): bigint {
  const ts = toBigInt(totalShares);
  if (ts === 0n) return 0n;
  return (toBigInt(shares) * toBigInt(totalAssetsAtoms)) / ts;
}

/**
 * Convert an atom amount into a vault-share quantity at the current share
 * price (mirrors the on-chain mint formula in `global_vault_deposit`).
 * For a genesis deposit pass `totalShares = 0`; it returns `atoms` 1:1.
 */
export function atomsToVaultShares(
  atoms: bigint | BN,
  totalShares: bigint | BN,
  totalAssetsAtoms: bigint | BN,
): bigint {
  const ts = toBigInt(totalShares);
  const tassets = toBigInt(totalAssetsAtoms);
  if (ts === 0n) return toBigInt(atoms);
  if (tassets === 0n) throw new RangeError('atomsToVaultShares: assets are 0 with non-zero shares');
  return (toBigInt(atoms) * ts) / tassets;
}

/** Shares of a vault deposit, normalised to a UI float using the debt mint's decimals. */
export function vaultSharesToUi(
  shares: bigint | BN,
  totalShares: bigint | BN,
  totalAssetsAtoms: bigint | BN,
  decimals: number,
): number {
  return atomsToUi(vaultSharesToAtoms(shares, totalShares, totalAssetsAtoms), decimals);
}

/* ─────────────────────────── helpers ────────────────────────────── */

function toBigInt(v: bigint | BN | number): bigint {
  if (typeof v === 'bigint') return v;
  if (typeof v === 'number') return BigInt(Math.trunc(v));
  return BigInt(v.toString());
}
