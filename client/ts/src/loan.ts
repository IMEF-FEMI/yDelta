import { Connection, PublicKey } from '@solana/web3.js';
import BN from 'bn.js';
import {
  LoanFixed,
  decodeLoanFixed,
} from './ydelta/accounts';
import {
  LoanState,
  LoanType,
  LenderKind,
} from './ydelta/types/enums';
import { loanPda } from './utils/pdas';
import { BPS_DENOMINATOR } from './utils/numbers';
import { SECONDS_PER_YEAR } from './utils/time';

export class Loan {
  readonly address: PublicKey;
  private _buffer: Buffer;
  private _data: LoanFixed;

  private constructor(address: PublicKey, buffer: Buffer) {
    this.address = address;
    this._buffer = buffer;
    this._data = decodeLoanFixed(buffer);
  }

  static async load({
    connection,
    market,
    sequence,
    programId,
  }: {
    connection: Connection;
    market: PublicKey;
    sequence: BN | number | bigint;
    programId?: PublicKey;
  }): Promise<Loan | null> {
    const seq = toSafeBigInt(sequence);
    const [pda] = loanPda(market, seq, programId);
    const info = await connection.getAccountInfo(pda);
    if (!info) return null;
    return new Loan(pda, Buffer.from(info.data));
  }

  static async loadFromAddress({
    connection,
    address,
  }: {
    connection: Connection;
    address: PublicKey;
  }): Promise<Loan | null> {
    const info = await connection.getAccountInfo(address);
    if (!info) return null;
    return new Loan(address, Buffer.from(info.data));
  }

  static loadFromBuffer({
    address,
    buffer,
  }: {
    address: PublicKey;
    buffer: Buffer;
  }): Loan {
    return new Loan(address, buffer);
  }

  async reload(connection: Connection): Promise<void> {
    const info = await connection.getAccountInfo(this.address);
    if (!info) throw new Error(`Loan ${this.address.toBase58()} not found`);
    this._buffer = Buffer.from(info.data);
    this._data = decodeLoanFixed(this._buffer);
  }

  data(): LoanFixed {
    return this._data;
  }

  market(): PublicKey {
    return this._data.market;
  }

  isActive(): boolean {
    return this._data.state === LoanState.Active;
  }

  isRepaid(): boolean {
    return this._data.state === LoanState.Repaid;
  }

  isP2Pool(): boolean {
    return this._data.loanType === LoanType.P2Pool;
  }

  isFixed(): boolean {
    return this._data.loanType === LoanType.Fixed;
  }

  isVaultFunded(): boolean {
    return this._data.lenderKind === LenderKind.Vault;
  }

  isSecondaryListed(): boolean {
    return this._data.hasRestingSecondaryBid;
  }

  /** Maturity timestamp (unix seconds). */
  maturesAt(): bigint {
    return BigInt(this._data.maturesAtUnix.toString());
  }

  /** Origination timestamp (unix seconds). */
  startedAt(): bigint {
    return BigInt(this._data.startedAtUnix.toString());
  }

  gracePeriodEnd(gracePeriodSeconds: number | bigint): bigint {
    return this.maturesAt() + BigInt(gracePeriodSeconds);
  }

  isMatured(nowUnix: number | bigint): boolean {
    return BigInt(nowUnix) > this.maturesAt();
  }

  isPastGrace(nowUnix: number | bigint, gracePeriodSeconds: number | bigint): boolean {
    return BigInt(nowUnix) > this.gracePeriodEnd(gracePeriodSeconds);
  }

  // ─── Accrual simulators ───
  //
  // Mirror `accrue_loan` in programs/ydelta/src/state/loan.rs:
  //   • simple interest on the IMMUTABLE `principal_debt_atoms` (not on
  //     `outstanding`, which already carries accrued interest — using it
  //     would compound),
  //   • accrues continuously through and past `matures_at_unix` at the
  //     original `borrower_rate_bps` / `lender_rate_bps` (settlement reads
  //     this same accrual; see test `accrue_post_maturity_uses_same_rate`),
  //   • no-op on `P2Pool` loans (their outstanding tracks marginfi share
  //     inflation directly via the borrow-CPI bookkeeping).
  //
  // For vault-funded loans the curator takes `curator_fee_bps_snapshot` out
  // of the lender's gross interest before it lands on `lender_claimable`.

  private elapsedSinceLastAccrue(nowUnix: number | bigint): BN {
    const last = bnToBigInt(this._data.lastAccruedUnix);
    const now = typeof nowUnix === 'bigint' ? nowUnix : BigInt(nowUnix);
    const delta = now - last;
    if (delta <= 0n) return new BN(0);
    return new BN(delta.toString());
  }

  /** Accrued borrower debt at `nowUnix`. */
  currentOutstanding(nowUnix: number | bigint): bigint {
    const outstanding = this._data.outstandingDebtAtoms;
    if (!this.isActive() || this.isP2Pool()) return bnToBigInt(outstanding);
    const elapsed = this.elapsedSinceLastAccrue(nowUnix);
    if (elapsed.isZero()) return bnToBigInt(outstanding);
    const interest = this._data.principalDebtAtoms
      .mul(new BN(this._data.borrowerRateBps))
      .mul(elapsed)
      .div(new BN(BPS_DENOMINATOR).mul(new BN(SECONDS_PER_YEAR)));
    return bnToBigInt(outstanding.add(interest));
  }

  /** Lender-side accrued claim at `nowUnix`. Net of the curator's cut on
   *  vault-funded loans (`lender_kind == Vault`). */
  lenderClaimable(nowUnix: number | bigint): bigint {
    const claimable = this._data.lenderClaimableAtoms;
    if (!this.isActive() || this.isP2Pool()) return bnToBigInt(claimable);
    const elapsed = this.elapsedSinceLastAccrue(nowUnix);
    if (elapsed.isZero()) return bnToBigInt(claimable);
    const lenderGross = this._data.principalDebtAtoms
      .mul(new BN(this._data.lenderRateBps))
      .mul(elapsed)
      .div(new BN(BPS_DENOMINATOR).mul(new BN(SECONDS_PER_YEAR)));
    let lenderNet = lenderGross;
    if (this.isVaultFunded() && this._data.curatorFeeBpsSnapshot > 0) {
      const curatorTake = lenderGross
        .mul(new BN(this._data.curatorFeeBpsSnapshot))
        .div(new BN(BPS_DENOMINATOR));
      lenderNet = lenderGross.sub(curatorTake);
    }
    return bnToBigInt(claimable.add(lenderNet));
  }

  /** Cumulative principal already paid down (atoms). */
  principalRetiredAtoms(): bigint {
    return bnToBigInt(this._data.principalRetiredAtoms);
  }

  /** Debt-bank share-price snapshot at origination (fp48). */
  lenderDebtSharePriceFp48(): bigint {
    return bnToBigInt(this._data.lenderDebtSharePriceSnapshotFp48);
  }

  /** Collateral-bank share-price snapshot at origination (fp48). */
  borrowerCollateralSharePriceFp48(): bigint {
    return bnToBigInt(this._data.borrowerCollateralSharePriceSnapshotFp48);
  }
}

function bnToBigInt(v: BN): bigint {
  return BigInt(v.toString());
}

function toSafeBigInt(v: BN | number | bigint): bigint {
  if (typeof v === 'bigint') return v;
  if (typeof v === 'number') {
    if (!Number.isSafeInteger(v)) {
      throw new Error(`Loan.load: sequence must be a safe integer (got ${v})`);
    }
    return BigInt(v);
  }
  return BigInt(v.toString());
}
