import { PublicKey } from '@solana/web3.js';
import BigNumber from 'bignumber.js';
import { Market } from '../market';
import { Loan } from '../loan';
import { convertP2PoolToFixedPreview } from '../helpers/preview';
import { MarginfiReader } from './client';
import { getP2PoolLiveLiability, getP2PoolCurrentBorrowApr } from './p2pool';

export type RefinanceSavings = {
  /** Atoms the borrower currently owes (marginfi live liability). */
  currentLiabilityAtoms: BigNumber;
  /** Marginfi's current borrow APR. */
  currentMarginfiApr: number;
  /** Best ask rate available in the orderbook (bps). */
  bestAskRateBps: number | null;
  /** Atom-weighted average rate of the asks that would cross at `maxAcceptableRateBps`. */
  weightedFixedRateBps: number;
  /** Atoms that would be filled at the fixed rate (from `convertP2PoolToFixedPreview`). */
  fillableAtoms: BigNumber;
  /** Atoms that would stay on P2Pool (residual). */
  residualAtoms: BigNumber;
  /** Projected interest cost OVER REMAINING TERM if we stay on P2Pool. */
  projectedP2PoolInterestAtoms: BigNumber;
  /** Projected interest cost OVER REMAINING TERM after converting (fixed portion +
   *  residual still on P2Pool at current marginfi rate). */
  projectedFixedInterestAtoms: BigNumber;
  /** P2Pool − Fixed projected cost. Positive means converting saves the borrower money. */
  estimatedSavingsAtoms: BigNumber;
  /** Recommended `maxAcceptableRateBps` to pass to `ConvertP2PoolToFixed`. */
  recommendedMaxAcceptableRateBps: number;
  /** Did anything fill at all? */
  anyFill: boolean;
};

/** End-to-end refinance preview combining yDelta's matching simulation with
 *  marginfi's live rate. Returns enough to power a "Convert to Fixed" CTA on
 *  a P2Pool loan dashboard. */
export async function previewRefinanceSavings({
  reader,
  market,
  loan,
  debtBank,
  /** Optional override; defaults to "anything below current marginfi rate". */
  maxAcceptableRateBps,
  /** When the user wants to explore a tighter cap, pass it here. */
  ratePaddingBps = 0,
  nowUnix = Math.floor(Date.now() / 1000),
}: {
  reader: MarginfiReader;
  market: Market;
  loan: Loan;
  debtBank: PublicKey;
  maxAcceptableRateBps?: number;
  ratePaddingBps?: number;
  nowUnix?: number;
}): Promise<RefinanceSavings> {
  const live = await getP2PoolLiveLiability(reader, loan, debtBank);
  const currentMarginfiApr = await getP2PoolCurrentBorrowApr(reader, debtBank);
  // marginfi rate is continuous; convert to bps for comparison with yDelta's
  // rate field (which is annualised simple bps).
  const marginfiRateBps = Math.floor(currentMarginfiApr * 10_000);
  const cap =
    maxAcceptableRateBps !== undefined
      ? maxAcceptableRateBps
      : Math.max(0, marginfiRateBps - ratePaddingBps);

  const preview = convertP2PoolToFixedPreview({
    market,
    loan,
    maxAcceptableRateBps: cap,
    nowUnix,
  });

  const bestAsk = market.bestAsk();
  const remainingTermSeconds = Math.max(0, Number(loan.maturesAt()) - nowUnix);
  const termYearFraction = remainingTermSeconds / 31_536_000;

  // Project interest the borrower would pay over remaining term, two ways.
  const fillableAtoms = new BigNumber(preview.totalFilled.toString());
  const residualAtoms = new BigNumber(preview.residualPrincipal.toString());

  // P2Pool branch: full current liability accrues at marginfi rate.
  const p2poolCost = live.liabilityAtoms.multipliedBy(currentMarginfiApr).multipliedBy(termYearFraction);

  // Fixed branch: filled portion accrues at the weighted-average ask rate
  // (yDelta uses simple bps so we treat as APR directly); residual stays on
  // P2Pool at marginfi rate.
  const weightedAprFromBps = preview.weightedAvgRateBps / 10_000;
  const fixedFilledCost = fillableAtoms.multipliedBy(weightedAprFromBps).multipliedBy(termYearFraction);
  const fixedResidualCost = residualAtoms.multipliedBy(currentMarginfiApr).multipliedBy(termYearFraction);

  const fixedCost = fixedFilledCost.plus(fixedResidualCost);
  const savings = p2poolCost.minus(fixedCost);

  return {
    currentLiabilityAtoms: live.liabilityAtoms,
    currentMarginfiApr,
    bestAskRateBps: bestAsk?.rateBps ?? null,
    weightedFixedRateBps: preview.weightedAvgRateBps,
    fillableAtoms,
    residualAtoms,
    projectedP2PoolInterestAtoms: p2poolCost,
    projectedFixedInterestAtoms: fixedCost,
    estimatedSavingsAtoms: savings,
    recommendedMaxAcceptableRateBps: cap,
    anyFill: preview.fills.length > 0,
  };
}
