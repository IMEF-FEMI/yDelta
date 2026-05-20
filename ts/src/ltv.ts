// Match-time LTV math. Port of
// programs/ydelta/src/state/ltv.rs::get_required_quote_collateral_to_back_debt.
import { FP48_SHIFT } from './constants.js';

const FP48_ONE = 1n << FP48_SHIFT;
const U64_MAX = (1n << 64n) - 1n;

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

export interface LtvInputs {
  borrowAtoms: bigint;
  debtPriceFp48: bigint;
  collateralPriceFp48: bigint;
  liabilityWeightInitFp48: bigint;
  collateralAssetWeightInitFp48: bigint;
  ltvBufferBps: number;
  debtMintDecimals: number;
  collateralMintDecimals: number;
}

// required = ceil(
//   borrow × debt_price × liability_weight × (1 + buffer/10_000)
//   / (collateral_price × asset_weight)
//   × 10^(coll_dec − debt_dec)
// )
// Returns u64::MAX on degenerate collateral side; the match gate's
// `collateral >= required` then rejects.
export function requiredCollateralAtoms(args: LtvInputs): bigint {
  if (args.collateralPriceFp48 === 0n || args.collateralAssetWeightInitFp48 === 0n) {
    return U64_MAX;
  }

  const debtAtomsFp48 = toFp48(args.borrowAtoms);
  const num1 = mulScale(debtAtomsFp48, args.debtPriceFp48);
  const num2 = mulScale(num1, args.liabilityWeightInitFp48);

  const buffered =
    args.ltvBufferBps > 0
      ? (num2 * (10_000n + BigInt(args.ltvBufferBps))) / 10_000n
      : num2;

  const denom = mulScale(args.collateralPriceFp48, args.collateralAssetWeightInitFp48);
  const resultFp48 = divScale(buffered, denom);

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
// more than they can safely draw.
export function maxBorrowAtoms(args: MaxBorrowInputs): bigint {
  if (args.debtPriceFp48 === 0n || args.liabilityWeightInitFp48 === 0n) {
    return 0n;
  }

  const collAtomsFp48 = toFp48(args.collateralAtoms);
  const num1 = mulScale(collAtomsFp48, args.collateralPriceFp48);
  const num2 = mulScale(num1, args.collateralAssetWeightInitFp48);

  const debuffered =
    args.ltvBufferBps > 0
      ? (num2 * 10_000n) / (10_000n + BigInt(args.ltvBufferBps))
      : num2;

  const denom = mulScale(args.debtPriceFp48, args.liabilityWeightInitFp48);
  const resultFp48 = divScale(debuffered, denom);

  const decDiff = args.debtMintDecimals - args.collateralMintDecimals;
  const normalizedFp48 =
    decDiff >= 0 ? resultFp48 * pow10(decDiff) : resultFp48 / pow10(-decDiff);

  const atoms = normalizedFp48 >> FP48_SHIFT;
  return atoms > U64_MAX ? U64_MAX : atoms;
}

// Bps representation of the LTV a (principal, collateral) pair would record.
// Lower = safer. Plug into simulatePlaceOrder.ltvEstimator for oracle-real
// preflight against profile.max_ltv_bps.
export function actualLtvBps(args: {
  principalAtoms: bigint;
  collateralAtoms: bigint;
  debtPriceFp48: bigint;
  collateralPriceFp48: bigint;
  liabilityWeightInitFp48: bigint;
  collateralAssetWeightInitFp48: bigint;
  debtMintDecimals: number;
  collateralMintDecimals: number;
}): number {
  if (args.collateralAtoms === 0n) return Number.MAX_SAFE_INTEGER;
  const required = requiredCollateralAtoms({
    borrowAtoms: args.principalAtoms,
    debtPriceFp48: args.debtPriceFp48,
    collateralPriceFp48: args.collateralPriceFp48,
    liabilityWeightInitFp48: args.liabilityWeightInitFp48,
    collateralAssetWeightInitFp48: args.collateralAssetWeightInitFp48,
    ltvBufferBps: 0,
    debtMintDecimals: args.debtMintDecimals,
    collateralMintDecimals: args.collateralMintDecimals,
  });
  return Number((required * 10_000n) / args.collateralAtoms);
}
