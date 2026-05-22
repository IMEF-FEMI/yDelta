import 'dotenv/config';
import { Connection, PublicKey } from '@solana/web3.js';
import {
  fetchMarket,
  decodeBank,
  OwnerKind,
  decodeUserAccount,
  userAccountPda,
  decodeMarginfiAccount,
  findActiveBalance,
  borrowerIntegrationAccountPda,
} from '../src/index.js';

const OWNER = new PublicKey('6pGqYzmEFVQBgiurnrrbYMwGENHUr7GigrhYXjjP7Jzf');
const MARKETS: Record<string, string> = {
  'USDC/SOL': '5o3ixfD6fmPhW7rVTTL25sy9PKLrFmHHQtGBsiSm9T3B',
  'USDC/JitoSOL': 'HyAFntxuz61JveNvqMXBRii5eyakz65XRbuSceLjP2ts',
};

const sharesToAtoms = (shares: bigint, sv: bigint): bigint => (shares * sv) >> 96n;
const fmt = (atoms: bigint, dec: number): string => (Number(atoms) / 10 ** dec).toFixed(6);

async function main(): Promise<void> {
  const url = process.env.YDELTA_RPC_URL ?? process.env.RPC_HTTP_URL ?? 'https://api.mainnet-beta.solana.com';
  const conn = new Connection(url, 'confirmed');
  console.log('owner:', OWNER.toBase58());

  for (const [label, addr] of Object.entries(MARKETS)) {
    const marketPk = new PublicKey(addr);
    const m = await fetchMarket(conn, marketPk);
    console.log(`\n=== ${label} (${addr}) ===`);
    if (!m) {
      console.log('  market not found');
      continue;
    }
    const h = m.header;
    const banks = await conn.getMultipleAccountsInfo([h.debtLendingPool, h.collateralLendingPool]);
    const debtBank = decodeBank(banks[0]!.data);
    const collBank = decodeBank(banks[1]!.data);
    const collDec = h.collateralMintDecimals;
    const debtDec = h.debtMintDecimals;

    const seat = m.claimedSeats.find(
      (s) => s.seat.ownerKind === OwnerKind.User && s.seat.owner.equals(OWNER),
    );
    if (!seat) {
      console.log('  no User seat for this owner');
      continue;
    }
    // Un-promoted matched loans for this borrower seat (cranker never ran).
    const mine = m.matchedLoans.filter((ml) => ml.loan.borrowerSeatIndex === seat.index);
    console.log(`  matched-loan queue entries for this seat: ${mine.length}`);
    for (const { loan: ml } of mine) {
      console.log(
        `    seq=${ml.sequence} type=${ml.loanType === 1 ? 'P2Pool' : 'Fixed'} principal=${ml.principalAtoms} collateral=${ml.collateralAtoms} rate=${ml.borrowerRateBps}bps`,
      );
    }
    const s = seat.seat;
    const csv = collBank.assetShareValueFp48;
    const dsv = debtBank.assetShareValueFp48;
    console.log('  asv collateral fp48:', csv.toString());
    console.log('  asv debt       fp48:', dsv.toString());
    console.log(
      `  collateral  withdrawable: shares=${s.collateralWithdrawableShares}  → ${fmt(sharesToAtoms(s.collateralWithdrawableShares, csv), collDec)}`,
    );
    console.log(
      `  collateral  encumbered  : shares=${s.collateralEncumberedShares}  → ${fmt(sharesToAtoms(s.collateralEncumberedShares, csv), collDec)}`,
    );
    console.log(
      `  debt        withdrawable: shares=${s.debtWithdrawableShares}  → ${fmt(sharesToAtoms(s.debtWithdrawableShares, dsv), debtDec)}`,
    );
    console.log(
      `  debt        encumbered  : shares=${s.debtEncumberedShares}  → ${fmt(sharesToAtoms(s.debtEncumberedShares, dsv), debtDec)}`,
    );
    console.log(`  openBorrowCount=${s.openBorrowCount} openLendCount=${s.openLendCount}`);

    // Outstanding marginfi borrow on the market's borrower integration account.
    const borrowerMa = borrowerIntegrationAccountPda(marketPk)[0];
    const maInfo = await conn.getAccountInfo(borrowerMa);
    if (maInfo) {
      const ma = decodeMarginfiAccount(maInfo.data);
      const bal = findActiveBalance(ma, h.debtLendingPool);
      const liabShares = bal?.liabilitySharesFp48 ?? 0n;
      const lsv = debtBank.liabilityShareValueFp48;
      const liabAtoms = (liabShares * lsv) >> 96n;
      console.log(
        `  marginfi borrower acct ${borrowerMa.toBase58().slice(0, 6)}: debt liability shares=${liabShares} → ${fmt(liabAtoms, debtDec)} USDC owed`,
      );
    }
  }

  // Open loans recorded on the user's UserAccount mirror.
  const uaPk = userAccountPda(OWNER)[0];
  const uaInfo = await conn.getAccountInfo(uaPk);
  console.log(`\n=== UserAccount ${uaPk.toBase58()} ===`);
  if (!uaInfo) {
    console.log('  no UserAccount');
  } else {
    const ua = decodeUserAccount(uaInfo.data);
    console.log(`  openLoanCount=${ua.header.openLoanCount}`);
    for (const l of ua.openLoans) {
      console.log(
        `  loan ${l.loan.toBase58()} market=${l.market.toBase58().slice(0, 6)} principal=${l.principalAtoms} rate=${l.rateBps}bps role=${l.role} cpKind=${l.counterpartyKind} matures=${l.maturesAtUnix}`,
      );
    }
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
