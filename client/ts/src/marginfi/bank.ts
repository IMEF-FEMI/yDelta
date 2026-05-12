import { PublicKey } from '@solana/web3.js';
import BigNumber from 'bignumber.js';
import {
  computeInterestRates,
  computeRemainingCapacity,
  computeUtilizationRate,
  getAssetQuantity,
  getLiabilityQuantity,
  getTotalAssetQuantity,
  getTotalLiabilityQuantity,
  MarginRequirementType,
  OraclePrice,
  PriceBias,
} from '@mrgnlabs/marginfi-client-v2';
import { MarginfiReader } from './client';
import { BankView } from './bank-view';

/** Marginfi-client-v2's pure helpers consume a duck-typed `bank` /
 *  `bank.config` parameter. Our `BankView` matches that shape (same
 *  field names, BigNumber values), so casting to `any` at the call
 *  boundary is intentional — TS can't unify our local types with the
 *  package's internal types, but the runtime shape is identical. */
type AnyBank = any;

export type BankRates = {
  /** Annualised deposit APR — what marginfi LPs earn for supplying. */
  supplyApr: number;
  /** Annualised borrow APR — what borrowers pay. */
  borrowApr: number;
  /** Continuous-compound translation (same math marginfi's UI uses). */
  supplyApy: number;
  borrowApy: number;
  /** `total_liability_atoms / total_asset_atoms` in [0,1]. */
  utilization: number;
};

/** Live rate + utilization snapshot for a marginfi bank. */
export async function getBankRates(reader: MarginfiReader, bank: PublicKey): Promise<BankRates> {
  const b = await reader.loadBank(bank);
  const { lendingRate, borrowingRate } = computeInterestRates(b as AnyBank);
  const utilization = computeUtilizationRate(b as AnyBank);
  return {
    supplyApr: lendingRate.toNumber(),
    borrowApr: borrowingRate.toNumber(),
    supplyApy: aprToApy(lendingRate.toNumber()),
    borrowApy: aprToApy(borrowingRate.toNumber()),
    utilization: utilization.toNumber(),
  };
}

export type BankLiquidity = {
  /** Atoms currently supplied (asset side). */
  totalAssetAtoms: BigNumber;
  /** Atoms currently borrowed (liability side). */
  totalLiabilityAtoms: BigNumber;
  /** Free atoms remaining = assets - liabilities. */
  freeAtoms: BigNumber;
  /** Atoms more that can be deposited before hitting the configured cap. */
  depositCapacityAtoms: BigNumber;
  /** Atoms more that can be borrowed before hitting the configured cap. */
  borrowCapacityAtoms: BigNumber;
};

/** Aggregate deposit/borrow totals + remaining headroom for a bank. */
export async function getBankLiquidity(
  reader: MarginfiReader,
  bank: PublicKey,
): Promise<BankLiquidity> {
  const b = await reader.loadBank(bank);
  const totalAssetAtoms = getTotalAssetQuantity(b as AnyBank);
  const totalLiabilityAtoms = getTotalLiabilityQuantity(b as AnyBank);
  const cap = computeRemainingCapacity(b as AnyBank);
  return {
    totalAssetAtoms,
    totalLiabilityAtoms,
    freeAtoms: BigNumber.max(totalAssetAtoms.minus(totalLiabilityAtoms), new BigNumber(0)),
    depositCapacityAtoms: cap.depositCapacity,
    borrowCapacityAtoms: cap.borrowCapacity,
  };
}

export type BankRiskParams = {
  /** Asset weight (init) — multiplier on collateral USD value for the
   *  init-tier health check. */
  assetWeightInit: number;
  /** Asset weight (maint). */
  assetWeightMaint: number;
  /** Liability weight (init) — multiplier on debt USD value. */
  liabilityWeightInit: number;
  /** Liability weight (maint). */
  liabilityWeightMaint: number;
};

/** Per-bank marginfi weights. The pairwise LTV is composite —
 *  `init_ltv = collateral.assetWeightInit / debt.liabilityWeightInit` — and
 *  must be computed from both banks; this helper deliberately does not
 *  return a single-bank LTV. */
export async function getBankRiskParams(
  reader: MarginfiReader,
  bank: PublicKey,
): Promise<BankRiskParams> {
  const b = await reader.loadBank(bank);
  const cfg = b.config;
  return {
    assetWeightInit: cfg.assetWeightInit.toNumber(),
    assetWeightMaint: cfg.assetWeightMaint.toNumber(),
    liabilityWeightInit: cfg.liabilityWeightInit.toNumber(),
    liabilityWeightMaint: cfg.liabilityWeightMaint.toNumber(),
  };
}

/** Pairwise init/maint LTV for a `(collateral, debt)` bank pair. */
export async function getPairwiseLtv(
  reader: MarginfiReader,
  collateralBank: PublicKey,
  debtBank: PublicKey,
): Promise<{ initLtv: number; maintenanceLtv: number }> {
  const [c, d] = await Promise.all([reader.loadBank(collateralBank), reader.loadBank(debtBank)]);
  return {
    initLtv: c.config.assetWeightInit.div(d.config.liabilityWeightInit).toNumber(),
    maintenanceLtv: c.config.assetWeightMaint.div(d.config.liabilityWeightMaint).toNumber(),
  };
}

/** Convert a share quantity to atoms via the bank's `assetShareValue`.
 *  Convenience around marginfi-client-v2's exported `getAssetQuantity`. */
export function bankAssetQuantity(bank: BankView, shares: BigNumber): BigNumber {
  return getAssetQuantity(bank as AnyBank, shares);
}

/** Convert a share quantity to atoms via the bank's `liabilityShareValue`. */
export function bankLiabilityQuantity(bank: BankView, shares: BigNumber): BigNumber {
  return getLiabilityQuantity(bank as AnyBank, shares);
}

/** USD value of a `shares` amount on a bank, given a cached oracle price.
 *  Use `MarginfiReader.setSpotPrice(bank, usd)` to wire the price first.
 *
 *  This builds the USD value the same way marginfi-client-v2's
 *  `computeAssetUsdValue` / `computeLiabilityUsdValue` would, but
 *  inlined here so we don't pull in the broken `Bank` class.
 *  `margin == Equity` skips the weight multiplier (which marginfi
 *  applies only on Initial/Maintenance tiers). */
export function bankAssetUsdValue(
  reader: MarginfiReader,
  bank: BankView,
  shares: BigNumber,
  margin: MarginRequirementType = MarginRequirementType.Equity,
): BigNumber {
  const price = reader.oraclePriceFor(bank.address);
  if (!price) throw new Error(`no oracle price wired for bank ${bank.address.toBase58()}`);
  const atoms = bankAssetQuantity(bank, shares);
  const weight = weightForMargin(bank.config.assetWeightInit, bank.config.assetWeightMaint, margin);
  return atoms
    .times(getPriceValue(price, PriceBias.None))
    .times(weight)
    .dividedBy(new BigNumber(10).pow(bank.mintDecimals));
}

export function bankLiabilityUsdValue(
  reader: MarginfiReader,
  bank: BankView,
  shares: BigNumber,
  margin: MarginRequirementType = MarginRequirementType.Equity,
): BigNumber {
  const price = reader.oraclePriceFor(bank.address);
  if (!price) throw new Error(`no oracle price wired for bank ${bank.address.toBase58()}`);
  const atoms = bankLiabilityQuantity(bank, shares);
  const weight = weightForMargin(
    bank.config.liabilityWeightInit,
    bank.config.liabilityWeightMaint,
    margin,
  );
  return atoms
    .times(getPriceValue(price, PriceBias.None))
    .times(weight)
    .dividedBy(new BigNumber(10).pow(bank.mintDecimals));
}

function weightForMargin(init: BigNumber, maint: BigNumber, m: MarginRequirementType): BigNumber {
  switch (m) {
    case MarginRequirementType.Initial:
      return init;
    case MarginRequirementType.Maintenance:
      return maint;
    case MarginRequirementType.Equity:
    default:
      return new BigNumber(1);
  }
}

function getPriceValue(p: OraclePrice, bias: PriceBias): BigNumber {
  // Mirrors `services/price/utils::getPrice` — `PriceBias.None` uses
  // the realtime point estimate, Lowest/Highest cross the band.
  switch (bias) {
    case PriceBias.Lowest:
      return p.priceWeighted.lowestPrice;
    case PriceBias.Highest:
      return p.priceWeighted.highestPrice;
    case PriceBias.None:
    default:
      return p.priceRealtime.price;
  }
}

/** APR → APY conversion (continuous compounding). */
export function aprToApy(apr: number): number {
  return Math.expm1(apr);
}
