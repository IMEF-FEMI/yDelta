import { PublicKey } from '@solana/web3.js';
import BigNumber from 'bignumber.js';
import BN from 'bn.js';
import { getLiabilityQuantity } from '@mrgnlabs/marginfi-client-v2';
import { Loan } from '../loan';
import { Market } from '../market';
import { LoanType } from '../ydelta/types/enums';
import { MarginfiReader } from './client';
import { getBankRates } from './bank';
import { I80F48_FRACTIONAL_BITS } from '../utils/numbers';

const I80F48_DENOM = new BigNumber(2).pow(I80F48_FRACTIONAL_BITS);

export type P2PoolLiveLiability = {
  /** Atoms the borrower currently owes marginfi (`borrow_shares × liability_share_value`).
   *  This is the LIVE amount — recomputes interest using marginfi's compounding model. */
  liabilityAtoms: BigNumber;
  /** Interest accrued since origination = liabilityAtoms - principal. */
  interestAccruedAtoms: BigNumber;
  /** The principal originally borrowed (loan body field). */
  principalAtoms: BigNumber;
  /** Effective per-second rate (continuous): liability / principal at age=t. */
  averageBorrowApr: number;
};

/** For a P2Pool loan: compute the current liability marginfi sees, using the
 *  bank's live `liability_share_value`. This is the "what do I actually owe
 *  right now" view that FE shows borrowers. */
export async function getP2PoolLiveLiability(
  reader: MarginfiReader,
  loan: Loan,
  debtBank: PublicKey,
): Promise<P2PoolLiveLiability> {
  if (loan.data().loanType !== LoanType.P2Pool) {
    throw new Error('getP2PoolLiveLiability called on non-P2Pool loan');
  }
  const bank = await reader.loadBank(debtBank);
  const shares = mantissaToShareDecimal(loan.data().borrowerMarginfiBorrowShares);
  // Use marginfi-client-v2's exported pure helper. Our `BankView` is
  // duck-shape compatible (assetShareValue / liabilityShareValue
  // BigNumber fields), so casting through `any` is intentional.
  const liabilityAtoms = getLiabilityQuantity(bank as any, shares);
  const principal = bnToBigNumber(loan.data().principalDebtAtoms);
  const accrued = BigNumber.max(liabilityAtoms.minus(principal), new BigNumber(0));
  const ageSeconds = Math.max(1, Math.floor(Date.now() / 1000) - Number(loan.startedAt()));
  // continuous compounding inverse: r = ln(L/P) / t
  const ratio = liabilityAtoms.div(principal.isZero() ? new BigNumber(1) : principal).toNumber();
  const averageBorrowApr = ratio > 0 ? (Math.log(ratio) * 31_536_000) / ageSeconds : 0;
  return {
    liabilityAtoms,
    interestAccruedAtoms: accrued,
    principalAtoms: principal,
    averageBorrowApr,
  };
}

/** What rate is marginfi currently charging on this debt bank? Same value the
 *  bank's borrowers see right now (subject to live utilization). */
export async function getP2PoolCurrentBorrowApr(
  reader: MarginfiReader,
  debtBank: PublicKey,
): Promise<number> {
  const rates = await getBankRates(reader, debtBank);
  return rates.borrowApr;
}

export type P2PoolPositionSummary = {
  loan: PublicKey;
  isP2Pool: true;
  /** Borrower's wallet — resolved from the market's `claimed_seats` at
   *  `loan.borrower_seat_index`. Not `loan.created_by` (= cranker). */
  borrower: PublicKey | null;
  collateralAtoms: BigNumber;
  /** Current marginfi liability. */
  live: P2PoolLiveLiability;
  /** Marginfi's current borrow rate. */
  currentMarginfiApr: number;
  /** How much more atoms the borrower would owe per year at current rate. */
  projectedAnnualInterestAtoms: BigNumber;
  /** Bank utilization — high values mean rate volatility. */
  utilizationFraction: number;
};

/** One-stop summary for a P2Pool loan dashboard tile. `market` is the parent
 *  market account — required so we can resolve the borrower's wallet via the
 *  `claimed_seats` tree (loan body only stores the seat index). */
export async function summarizeP2PoolPosition(
  reader: MarginfiReader,
  market: Market,
  loan: Loan,
  debtBank: PublicKey,
): Promise<P2PoolPositionSummary> {
  const live = await getP2PoolLiveLiability(reader, loan, debtBank);
  const rates = await getBankRates(reader, debtBank);
  const borrowerSeat = market.seatAtIndex(loan.data().borrowerSeatIndex);
  return {
    loan: loan.address,
    isP2Pool: true,
    borrower: borrowerSeat?.owner ?? null,
    collateralAtoms: bnToBigNumber(loan.data().collateralAtoms),
    live,
    currentMarginfiApr: rates.borrowApr,
    projectedAnnualInterestAtoms: live.liabilityAtoms.multipliedBy(rates.borrowApr),
    utilizationFraction: rates.utilization,
  };
}

function bnToBigNumber(v: BN): BigNumber {
  return new BigNumber(v.toString());
}

/** Convert the raw u128 I80F48 mantissa stored on-chain into a share quantity
 *  the marginfi SDK expects (mantissa / 2^48). */
function mantissaToShareDecimal(mantissa: BN): BigNumber {
  return new BigNumber(mantissa.toString()).dividedBy(I80F48_DENOM);
}
