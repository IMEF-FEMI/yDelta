/**
 * debug-seat-withdraw.ts — compare a user's seat.debt_withdrawable_shares
 * against what the per-market lender_marginfi_account actually holds, so we
 * can tell whether the marginfi `OperationWithdrawOnly` (code 6020) error
 * on seat-withdraw is a state inconsistency (seat > marginfi) or something
 * subtler.
 *
 * Usage:
 *   yarn tsx ts/scripts/debug-seat-withdraw.ts --market <pk> --owner <wallet>
 *   yarn tsx ts/scripts/debug-seat-withdraw.ts --market <pk>   # picks any user-seat with debt shares
 */
import 'dotenv/config';

import { Connection, PublicKey } from '@solana/web3.js';

import {
  decodeBank,
  decodeMarket,
  fetchMarketHeader,
  OwnerKind,
} from '../src/index.js';
import { lenderIntegrationAccountPda } from '../src/pdas.js';

const argv = process.argv.slice(2);
const arg = (k: string): string | undefined => {
  const i = argv.indexOf(k);
  return i >= 0 ? argv[i + 1] : undefined;
};
const marketArg = arg('--market');
if (!marketArg) {
  throw new Error('debug-seat-withdraw: --market <pubkey> is required (look it up in .local/markets.json)');
}
const MARKET = new PublicKey(marketArg);
const OWNER = arg('--owner') ? new PublicKey(arg('--owner')!) : null;
const RPC_URL =
  process.env.YDELTA_RPC_URL ?? process.env.RPC_URL ?? 'https://api.mainnet-beta.solana.com';

// marginfi v0.1.8 Marginfi account: balances live after the header.
// Each Balance entry is 104 bytes; balance[0] starts at body offset 64.
// Balance layout (the relevant fields):
//   @0   u8     active
//   @1   pubkey bank_pk          (32)
//   @33  u8     bank_asset_tag
//   @34  u16    tag
//   @40  I80F48 asset_shares     (16)  ← stored as i128 LE, but treated as I80F48
//   @56  I80F48 liability_shares (16)
const ANCHOR_DISC_LEN = 8;
const MFI_BALANCES_BODY_OFFSET = 64; // body offset (i.e. after the 8-byte anchor disc)
const MFI_MAX_BALANCES = 16;
const BALANCE_SIZE = 104;

function readI80F48Lo64Hi64(buf: Uint8Array, off: number): bigint {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  const lo = dv.getBigUint64(off, true);
  const hi = dv.getBigInt64(off + 8, true);
  // I80F48 stored as 128-bit signed; we know our shares are non-negative
  // (negative balances aren't possible for asset_shares in practice).
  const signed = (hi << 64n) | lo;
  return signed < 0n ? 0n : signed;
}

function readPubkey(buf: Uint8Array, off: number): PublicKey {
  return new PublicKey(new Uint8Array(buf.buffer, buf.byteOffset + off, 32));
}

interface MfiBalance {
  active: number;
  bankPk: PublicKey;
  assetShares: bigint; // I80F48-scaled (×2^48)
  liabilityShares: bigint;
}

function readMarginfiBalances(data: Uint8Array): MfiBalance[] {
  const out: MfiBalance[] = [];
  for (let i = 0; i < MFI_MAX_BALANCES; i++) {
    const off = ANCHOR_DISC_LEN + MFI_BALANCES_BODY_OFFSET + i * BALANCE_SIZE;
    if (off + BALANCE_SIZE > data.byteLength) break;
    const active = data[off];
    const bankPk = readPubkey(data, off + 1);
    const assetShares = readI80F48Lo64Hi64(data, off + 40);
    const liabilityShares = readI80F48Lo64Hi64(data, off + 56);
    out.push({ active, bankPk, assetShares, liabilityShares });
  }
  return out;
}

async function main() {
  const conn = new Connection(RPC_URL, 'confirmed');
  console.log(`RPC: ${RPC_URL}`);
  console.log(`Market: ${MARKET.toBase58()}`);

  const marketAcct = await conn.getAccountInfo(MARKET);
  if (!marketAcct) throw new Error('market not found');
  const market = decodeMarket(marketAcct.data);
  const header = market.header;
  console.log(`Debt: ${header.debtMint.toBase58()} (${header.debtMintDecimals} dec)`);
  console.log(`Coll: ${header.collateralMint.toBase58()} (${header.collateralMintDecimals} dec)`);

  // Find target seat.
  const userSeats = market.claimedSeats.filter((s) => s.seat.ownerKind === OwnerKind.User);
  const target = OWNER
    ? userSeats.find((s) => s.seat.owner.equals(OWNER))
    : userSeats.find((s) => s.seat.debtWithdrawableShares > 0n);
  if (!target) {
    console.log(`User seats (${userSeats.length}):`);
    for (const s of userSeats) {
      console.log(
        `  ${s.seat.owner.toBase58()}  debtWithdrawable=${s.seat.debtWithdrawableShares} collWithdrawable=${s.seat.collateralWithdrawableShares}`,
      );
    }
    throw new Error('no matching seat');
  }
  console.log(`\nSeat owner: ${target.seat.owner.toBase58()}`);
  console.log(`  debtWithdrawableShares    = ${target.seat.debtWithdrawableShares}  (I80F48-scaled)`);
  console.log(`  debtEncumberedShares      = ${target.seat.debtEncumberedShares}`);
  console.log(`  collateralWithdrawableShares = ${target.seat.collateralWithdrawableShares}`);
  console.log(`  collateralEncumberedShares   = ${target.seat.collateralEncumberedShares}`);

  // Read debt bank for share value.
  const debtBankAcct = await conn.getAccountInfo(header.debtLendingPool);
  if (!debtBankAcct) throw new Error('debt bank missing');
  const debtBank = decodeBank(debtBankAcct.data);
  console.log(`\nDebt bank: ${header.debtLendingPool.toBase58()}`);
  console.log(`  asset_share_value_fp48 = ${debtBank.assetShareValueFp48}`);

  // Compute seat's claimed atoms.
  // seat shares × share_value / 2^96 (both are × 2^48 scaled).
  const seatAtoms = (target.seat.debtWithdrawableShares * debtBank.assetShareValueFp48) >> 96n;
  console.log(`\nSeat → atoms (seat shares × share_value): ${seatAtoms}`);

  // Read lender_marginfi_account for the market.
  const [lenderMa] = lenderIntegrationAccountPda(MARKET);
  console.log(`\nLender marginfi account: ${lenderMa.toBase58()}`);
  const lenderMaAcct = await conn.getAccountInfo(lenderMa);
  if (!lenderMaAcct) throw new Error('lender marginfi account missing');
  const balances = readMarginfiBalances(lenderMaAcct.data);
  const debtBal = balances.find((b) => b.active !== 0 && b.bankPk.equals(header.debtLendingPool));
  if (!debtBal) {
    console.log(`  lender marginfi account has NO active balance for debt bank!`);
    console.log(`  active balances:`);
    for (const b of balances.filter((b) => b.active !== 0)) {
      console.log(
        `    bank=${b.bankPk.toBase58()} asset=${b.assetShares} liab=${b.liabilityShares}`,
      );
    }
    return;
  }
  console.log(`  debt-bank asset_shares  = ${debtBal.assetShares}  (I80F48-scaled)`);
  console.log(`  debt-bank liab_shares   = ${debtBal.liabilityShares}`);

  const lenderMaAtoms = (debtBal.assetShares * debtBank.assetShareValueFp48) >> 96n;
  console.log(`  lender marginfi → atoms = ${lenderMaAtoms}`);

  console.log(`\nDiff (seat - lenderMa shares):  ${target.seat.debtWithdrawableShares - debtBal.assetShares}`);
  console.log(`Diff (seat - lenderMa atoms):   ${seatAtoms - lenderMaAtoms}`);
  if (target.seat.debtWithdrawableShares > debtBal.assetShares) {
    console.log(
      `\n⚠️  STATE INCONSISTENCY: seat claims more shares than the per-market lender marginfi account holds.`,
    );
    console.log(
      `   Withdrawing seat.debt_withdrawable_shares would ask marginfi to give out more atoms than it has → OperationWithdrawOnly (6020).`,
    );
  } else {
    console.log(`\nNo seat-vs-marginfi share shortage. Withdraw should succeed — different cause.`);
  }
  // Also sanity-check the protocol-side total: sum of ALL user seats' debt
  // shares for this market vs lender-marginfi assets.
  const allSeatsDebtSharesSum = userSeats.reduce(
    (acc, s) => acc + s.seat.debtWithdrawableShares,
    0n,
  );
  console.log(
    `\nSum of all user seats' debt_withdrawable_shares (this market): ${allSeatsDebtSharesSum}`,
  );
  console.log(
    `Lender marginfi asset_shares: ${debtBal.assetShares}  diff=${allSeatsDebtSharesSum - debtBal.assetShares}`,
  );

  void fetchMarketHeader;
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
