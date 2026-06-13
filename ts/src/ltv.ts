// Match-time LTV math (v1). Port of
// programs/ydelta/src/state/ltv.rs::required_collateral_at_ltv_cap.
//
// v1 D17: Fixed-loan origination gates on the sub-vault's explicit
// `max_ltv_bps` cap — NOT on marginfi asset/liability weights, and NOT with
// any `ltv_buffer_bps` (the on-chain buffer field was removed entirely). The
// cap is fed in as a unit liability weight + a collateral weight of
// `cap_bps / 10_000`, so the requirement is the inverse of the cap:
//   required = ceil( debt_value / (cap × coll_price) )
// normalised across mint decimals, ceil-rounded. (marginfi maint weights
// still gate the P2Pool liquidation-health path on-chain, but that is not
// the origination sizing this module mirrors.)
import { FP48_SHIFT } from './constants.js';

const FP48_ONE = 1n << FP48_SHIFT;
const U64_MAX = (1n << 64n) - 1n;
const BPS_PER_UNIT = 10_000n;

export function toFp48(amount: bigint): bigint {
  return amount << FP48_SHIFT;
}

export function mulScale(a: bigint, b: bigint): bigint {
  return (a * b) >> FP48_SHIFT;
}

export function divScale(a: bigint, b: bigint): bigint {
  if (b === 0n) throw new RangeError('divScale: zero divisor');
  return (a << FP48_SHIFT) / b;
}

export function fromScaledCeil(scaled: bigint): bigint {
  const mask = FP48_ONE - 1n;
  const truncated = scaled >> FP48_SHIFT;
  return (scaled & mask) === 0n ? truncated : truncated + 1n;
}

function pow10(exp: number): bigint {
  let acc = 1n;
  for (let i = 0; i < exp; i++) acc *= 10n;
  return acc;
}

// Convert a sub-vault LTV cap in bps to the fp48 collateral weight the
// on-chain `required_collateral_at_ltv_cap` feeds in: `to_scaled(cap) /
// 10_000` = `(cap << 48) / 10_000`. Floors (matches the on-chain integer
// divide), so e.g. 8_000 bps weighs just under 0.8 and the requirement
// ceils one atom higher than a clean 0.8 — bit-for-bit with Rust.
function capWeightFp48(maxLtvBps: number): bigint {
  return (BigInt(maxLtvBps) << FP48_SHIFT) / BPS_PER_UNIT;
}

export interface LtvInputs {
  borrowAtoms: bigint;
  debtPriceFp48: bigint;
  collateralPriceFp48: bigint;
  /** Sub-vault origination LTV cap in bps (`SubVault.max_ltv_bps`). */
  maxLtvBps: number;
  debtMintDecimals: number;
  collateralMintDecimals: number;
}

// required = ceil(
//   borrow × debt_price × 1  (unit liability weight)
//   / (collateral_price × cap_weight)
//   × 10^(coll_dec − debt_dec)
// )
// where cap_weight = (max_ltv_bps << 48) / 10_000. Returns u64::MAX on a
// degenerate collateral side or a zero cap; the match gate's
// `collateral >= required` then rejects (fail-closed).
export function requiredCollateralAtoms(args: LtvInputs): bigint {
  const collWeightFp48 = capWeightFp48(args.maxLtvBps);
  if (
    args.collateralPriceFp48 === 0n ||
    collWeightFp48 === 0n ||
    args.debtPriceFp48 === 0n
  ) {
    return U64_MAX;
  }

  const debtAtomsFp48 = toFp48(args.borrowAtoms);
  const num1 = mulScale(debtAtomsFp48, args.debtPriceFp48);
  // Liability weight is unit (Fp48::ONE) in the cap formula, so num2 == num1.
  const num2 = mulScale(num1, FP48_ONE);

  const denom = mulScale(args.collateralPriceFp48, collWeightFp48);
  const resultFp48 = divScale(num2, denom);

  // Decimal normalisation applied in fp48 so the ceil below rounds the
  // TRUE required collateral up.
  const decDiff = args.collateralMintDecimals - args.debtMintDecimals;
  const normalizedFp48 =
    decDiff >= 0 ? resultFp48 * pow10(decDiff) : resultFp48 / pow10(-decDiff);

  const atoms = fromScaledCeil(normalizedFp48);
  return atoms > U64_MAX ? U64_MAX : atoms;
}

export interface MaxBorrowInputs extends Omit<LtvInputs, 'borrowAtoms'> {
  collateralAtoms: bigint;
}

// Inverse of requiredCollateralAtoms. Rounds DOWN — borrower never quoted
// more than the cap safely allows.
export function maxBorrowAtoms(args: MaxBorrowInputs): bigint {
  const collWeightFp48 = capWeightFp48(args.maxLtvBps);
  if (args.debtPriceFp48 === 0n || collWeightFp48 === 0n) {
    return 0n;
  }

  const collAtomsFp48 = toFp48(args.collateralAtoms);
  const num1 = mulScale(collAtomsFp48, args.collateralPriceFp48);
  const num2 = mulScale(num1, collWeightFp48);

  // Unit liability weight: denom == debt_price.
  const denom = mulScale(args.debtPriceFp48, FP48_ONE);
  const resultFp48 = divScale(num2, denom);

  const decDiff = args.debtMintDecimals - args.collateralMintDecimals;
  const normalizedFp48 =
    decDiff >= 0 ? resultFp48 * pow10(decDiff) : resultFp48 / pow10(-decDiff);

  const atoms = normalizedFp48 >> FP48_SHIFT;
  return atoms > U64_MAX ? U64_MAX : atoms;
}

// Bps representation of the position's TRUE oracle LTV (cap-independent):
//   ltv = debt_value / collateral_value × 10_000
// Lower = safer. Compare against `subVault.max_ltv_bps` for an oracle-real
// preflight; plug into simulatePlaceOrder.ltvEstimator. Values are
// normalised for mint-decimal differences in fp48 before the bps scale.
export function actualLtvBps(args: {
  principalAtoms: bigint;
  collateralAtoms: bigint;
  debtPriceFp48: bigint;
  collateralPriceFp48: bigint;
  debtMintDecimals: number;
  collateralMintDecimals: number;
}): number {
  if (args.collateralAtoms === 0n) return Number.MAX_SAFE_INTEGER;
  const debtValueFp48 = mulScale(toFp48(args.principalAtoms), args.debtPriceFp48);
  const collValueFp48 = mulScale(toFp48(args.collateralAtoms), args.collateralPriceFp48);
  if (collValueFp48 === 0n) return Number.MAX_SAFE_INTEGER;
  // Normalise both legs to the same atom basis before taking the ratio.
  const decDiff = args.collateralMintDecimals - args.debtMintDecimals;
  const normDebtFp48 =
    decDiff >= 0 ? debtValueFp48 * pow10(decDiff) : debtValueFp48 / pow10(-decDiff);
  return Number((normDebtFp48 * BPS_PER_UNIT) / collValueFp48);
}
