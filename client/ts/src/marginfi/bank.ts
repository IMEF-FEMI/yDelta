import { PublicKey } from '@solana/web3.js';
import BigNumber from 'bignumber.js';
import { Bank, MarginRequirementType, PriceBias } from '@mrgnlabs/marginfi-client-v2';
import { MarginfiReader } from './client';

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
export async function getBankRates(
  reader: MarginfiReader,
  bank: PublicKey,
): Promise<BankRates> {
  const b = await reader.loadBank(bank);
  const { lendingRate, borrowingRate } = b.computeInterestRates();
  const utilization = b.computeUtilizationRate();
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
  const totalAssetAtoms = b.getTotalAssetQuantity();
  const totalLiabilityAtoms = b.getTotalLiabilityQuantity();
  const cap = b.computeRemainingCapacity();
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

/** USD value of a `shares` amount on a bank, given a cached oracle price.
 *  Use `MarginfiReader.setSpotPrice(bank, usd)` to wire the price first. */
export function bankAssetUsdValue(
  reader: MarginfiReader,
  bank: Bank,
  shares: BigNumber,
  margin: MarginRequirementType = MarginRequirementType.Equity,
): BigNumber {
  const price = reader.oraclePriceFor(bank.address);
  if (!price) throw new Error(`no oracle price wired for bank ${bank.address.toBase58()}`);
  return bank.computeAssetUsdValue(price, shares, margin, PriceBias.None);
}

export function bankLiabilityUsdValue(
  reader: MarginfiReader,
  bank: Bank,
  shares: BigNumber,
  margin: MarginRequirementType = MarginRequirementType.Equity,
): BigNumber {
  const price = reader.oraclePriceFor(bank.address);
  if (!price) throw new Error(`no oracle price wired for bank ${bank.address.toBase58()}`);
  return bank.computeLiabilityUsdValue(price, shares, margin, PriceBias.None);
}

/** APR → APY conversion (continuous compounding). */
export function aprToApy(apr: number): number {
  return Math.expm1(apr);
}
