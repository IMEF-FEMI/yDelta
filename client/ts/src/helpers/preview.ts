import BN from 'bn.js';
import BigNumber from 'bignumber.js';
import { Market } from '../market';
import { Vault } from '../vault';
import { Loan } from '../loan';
import { OrderKind } from '../ydelta/types/enums';
import { FillPreview, MatchPreview } from './orderbook';

/** Preview the asks that would cross a P2Pool loan's `ConvertP2PoolToFixed`,
 *  plus the residual principal that remains on the P2Pool body. */
export function convertP2PoolToFixedPreview({
  market,
  loan,
  maxAcceptableRateBps,
  nowUnix,
  maxMakers,
}: {
  market: Market;
  loan: Loan;
  maxAcceptableRateBps: number;
  /** Unix-seconds for term-remaining calculation. */
  nowUnix: number;
  maxMakers?: number;
}): MatchPreview {
  if (!loan.isP2Pool()) {
    return {
      fills: [],
      totalFilled: new BN(0),
      residualPrincipal: loan.data().outstandingDebtAtoms,
      weightedAvgRateBps: 0,
    };
  }
  const remainingTerm = Math.max(0, Number(loan.maturesAt()) - nowUnix);
  let residual = loan.data().outstandingDebtAtoms.clone();
  const fills: FillPreview[] = [];
  let weightedSum = new BN(0);
  let walked = 0;
  for (const ask of market.iterAsks()) {
    if (maxMakers !== undefined && walked >= maxMakers) break;
    walked++;
    if (ask.kind !== OrderKind.Primary) continue;
    if (ask.rateBps > maxAcceptableRateBps) {
      // Asks are walked best-first (lowest rate). Once we exceed the cap,
      // every deeper ask has a higher rate and also fails.
      break;
    }
    if (ask.termSeconds < remainingTerm) continue;
    if (residual.lten(0)) break;
    const fill = BN.min(residual, ask.principalAtoms);
    fills.push({ maker: ask, filledAtoms: fill, rateBps: ask.rateBps });
    weightedSum = weightedSum.add(fill.muln(ask.rateBps));
    residual = residual.sub(fill);
  }
  const totalFilled = loan.data().outstandingDebtAtoms.sub(residual);
  return {
    fills,
    totalFilled,
    residualPrincipal: residual,
    weightedAvgRateBps: totalFilled.isZero() ? 0 : weightedSum.div(totalFilled).toNumber(),
  };
}

export type VaultRedemptionPreview = {
  atomsOut: BN;
  blockedByDeployment: boolean;
  /** Available idle atoms in the profile right now (without waiting for repayment). */
  idleAtomsAvailable: BN;
};

/** Preview a `GlobalVaultWithdraw`. Returns the atoms a depositor would receive
 *  for burning `sharesToBurn` of profile `profileId`, plus a flag indicating
 *  whether the request would exceed `idle_principal_atoms` (which is when the
 *  ix reverts with `VaultInsufficientIdleAtoms`). */
export function previewVaultRedemption({
  vault,
  profileId,
  sharesToBurn,
}: {
  vault: Vault;
  profileId: number;
  sharesToBurn: BN;
}): VaultRedemptionPreview {
  const profile = vault.getRiskProfile(profileId);
  if (!profile || profile.totalShares.isZero()) {
    return { atomsOut: new BN(0), blockedByDeployment: false, idleAtomsAvailable: new BN(0) };
  }
  // Shares → atoms: atoms = shares × total_assets_atoms / total_shares.
  const atomsOut = sharesToBurn.mul(profile.totalAssetsAtoms).div(profile.totalShares);
  const idle = vault.idleAtoms(profileId);
  return {
    atomsOut,
    blockedByDeployment: atomsOut.gt(idle),
    idleAtomsAvailable: idle,
  };
}

export type MaturedSettlePreview = {
  settleable: boolean;
  /** Unix-seconds after which keepers can act. */
  graceEndsAtUnix: bigint;
};

/** Preview whether `SettleMaturedLoan` would succeed right now. */
export function previewSettleMatured({
  loan,
  market,
  nowUnix,
}: {
  loan: Loan;
  market: Market;
  nowUnix: number;
}): MaturedSettlePreview {
  const grace = market.feeConfig().gracePeriodSeconds;
  const graceEnd = loan.gracePeriodEnd(grace);
  return {
    settleable: BigInt(nowUnix) > graceEnd && loan.isActive(),
    graceEndsAtUnix: graceEnd,
  };
}

export type LiquidationSimResult = {
  /** Approximate `loan_value / collateral_value` in basis points. */
  ltvBps: number;
  /** True when `ltvBps > maintenance_ltv_bps`. */
  liquidatable: boolean;
  /** Keeper bonus the SDK estimates the liquidator would collect (atoms of collateral). */
  estKeeperBonusAtoms: BN;
};

/** Pure-TS LTV simulation. Caller supplies oracle prices and the bank's
 *  maintenance LTV (basis points). For a definitive answer use
 *  `CheckLtvLiquidatable` (tag 40) via `simulateTransaction`. */
export function simulateLiquidation({
  loan,
  market,
  nowUnix,
  debtPriceUsd,
  collateralPriceUsd,
  debtMintDecimals,
  collateralMintDecimals,
  maintenanceLtvBps,
}: {
  loan: Loan;
  market: Market;
  nowUnix: number;
  debtPriceUsd: number;
  collateralPriceUsd: number;
  debtMintDecimals: number;
  collateralMintDecimals: number;
  /** marginfi bank's maintenance LTV in bps for this collateral. */
  maintenanceLtvBps: number;
}): LiquidationSimResult {
  // Use BigNumber for the atom→USD math: `outstanding` and `collateral` can
  // be 64-bit and overflow `Number`'s 53-bit mantissa for whale positions.
  const outstanding = new BigNumber(loan.currentOutstanding(nowUnix).toString());
  const collateral = new BigNumber(loan.data().collateralAtoms.toString());
  const debtScale = new BigNumber(10).pow(debtMintDecimals);
  const collScale = new BigNumber(10).pow(collateralMintDecimals);
  const debtValueUsd = outstanding.div(debtScale).multipliedBy(debtPriceUsd);
  const collValueUsd = collateral.div(collScale).multipliedBy(collateralPriceUsd);
  if (collValueUsd.lte(0)) {
    return {
      ltvBps: 10_000,
      liquidatable: true,
      estKeeperBonusAtoms: new BN(0),
    };
  }
  const ltvBps = debtValueUsd.div(collValueUsd).multipliedBy(10_000).integerValue(BigNumber.ROUND_FLOOR).toNumber();
  const liquidatable = ltvBps > maintenanceLtvBps;
  const bonusBps = market.feeConfig().liquidationKeeperBps;
  // Keeper bonus base matches Rust `compute_collateral_split` in
  // `programs/ydelta/src/program/processor/liquidate_loan.rs`:
  //   bonus_atoms = debt_value_in_collateral_atoms × bonus_bps / 10_000
  // where `debt_value_in_collateral_atoms` is the oracle-priced swap value
  // of the outstanding debt expressed in collateral atoms. NOT the full
  // posted collateral — for over-collateralised loans, multiplying by full
  // collateral overstates the bonus by up to 2× or more.
  let estKeeperBonusAtoms = new BN(0);
  if (liquidatable && bonusBps > 0) {
    const debtValueInCollateralAtoms = debtValueUsd
      .div(collateralPriceUsd)
      .multipliedBy(collScale)
      .integerValue(BigNumber.ROUND_FLOOR);
    const seizable = BigNumber.min(debtValueInCollateralAtoms, collateral);
    const bonus = seizable.multipliedBy(bonusBps).dividedBy(10_000).integerValue(BigNumber.ROUND_FLOOR);
    estKeeperBonusAtoms = new BN(bonus.toString(10));
  }
  return { ltvBps, liquidatable, estKeeperBonusAtoms };
}
