/**
 * debug-convert-tx.ts — fetch a live P2Pool loan, build the
 * convert_p2pool_to_fixed tx via the SDK as the UI does, dry-run via
 * `simulateTransaction`. The sim log shows whether the SDK is producing
 * the right account list — and where it falls short if not.
 *
 * Usage:
 *   yarn tsx ts/scripts/debug-convert-tx.ts --loan <pubkey>
 *   yarn tsx ts/scripts/debug-convert-tx.ts                      # picks first active P2Pool loan
 */
import 'dotenv/config';

import {
  ComputeBudgetProgram,
  Connection,
  PublicKey,
  TransactionMessage,
  VersionedTransaction,
} from '@solana/web3.js';
import bs58 from 'bs58';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  convertP2poolToFixedInstruction,
  decodeBank,
  decodeLoanFixed,
  fetchMarketHeader,
  LoanType,
  LOAN_FIXED_DISCRIMINANT,
  LOAN_FIXED_SIZE,
  LoanState,
  lookupClaimedSeatAt,
  OracleSetup,
  YDELTA_PROGRAM_ID,
} from '../src/index.js';

function oracleCountFor(setup: OracleSetup): number {
  switch (setup) {
    case OracleSetup.PythPushOracle:
    case OracleSetup.SwitchboardPull:
      return 1;
    case OracleSetup.StakedWithPythPush:
      return 3;
    default:
      return 1;
  }
}
import { MARGINFI_PROGRAM_ID } from './_marginfi.js';

const argv = process.argv.slice(2);
const arg = (k: string): string | undefined => {
  const i = argv.indexOf(k);
  return i >= 0 ? argv[i + 1] : undefined;
};

const RPC_URL =
  process.env.YDELTA_RPC_URL ?? process.env.RPC_URL ?? 'https://api.mainnet-beta.solana.com';

async function findP2PoolLoan(conn: Connection): Promise<PublicKey | null> {
  const disc = new Uint8Array(8);
  new DataView(disc.buffer).setBigUint64(0, LOAN_FIXED_DISCRIMINANT, true);
  const accs = await conn.getProgramAccounts(YDELTA_PROGRAM_ID, {
    filters: [
      { dataSize: LOAN_FIXED_SIZE },
      { memcmp: { offset: 0, bytes: bs58.encode(disc) } },
    ],
  });
  console.log(`Scanning ${accs.length} loan account(s)…`);
  for (const { pubkey, account } of accs) {
    let loan;
    try {
      loan = decodeLoanFixed(account.data);
    } catch {
      continue;
    }
    console.log(
      `  ${pubkey.toBase58()}  state=${loan.state} type=${loan.loanType} outstanding=${loan.outstandingDebtAtoms}`,
    );
    if (loan.state !== LoanState.Active) continue;
    if (loan.loanType !== LoanType.P2Pool) continue;
    return pubkey;
  }
  return null;
}

async function main() {
  const conn = new Connection(RPC_URL, 'confirmed');
  console.log(`RPC: ${RPC_URL}`);

  let loanPk: PublicKey | null = null;
  const loanArg = arg('--loan');
  if (loanArg) {
    loanPk = new PublicKey(loanArg);
  } else {
    console.log('No --loan passed; picking any active P2Pool loan…');
    loanPk = await findP2PoolLoan(conn);
    if (!loanPk) throw new Error('No active P2Pool loan found');
  }
  console.log(`Loan: ${loanPk.toBase58()}`);

  const loanAcct = await conn.getAccountInfo(loanPk);
  if (!loanAcct) throw new Error('loan account missing');
  const loan = decodeLoanFixed(loanAcct.data);
  console.log(`  state=${loan.state} type=${loan.loanType} sequence=${loan.matchedLoanSequence}`);
  console.log(`  market=${loan.market.toBase58()} borrowerSeatIndex=${loan.borrowerSeatIndex}`);

  const marketAcct = await conn.getAccountInfo(loan.market);
  if (!marketAcct) throw new Error('market account missing');
  const borrowerSeat = lookupClaimedSeatAt(marketAcct.data, loan.borrowerSeatIndex);
  if (!borrowerSeat) throw new Error('borrower seat lookup failed');
  console.log(`  borrower (from seat)=${borrowerSeat.owner.toBase58()}`);

  const header = await fetchMarketHeader(conn, loan.market);
  if (!header) throw new Error('market header missing');

  const [debtBankAcct, collBankAcct] = await conn.getMultipleAccountsInfo([
    header.debtLendingPool,
    header.collateralLendingPool,
  ]);
  if (!debtBankAcct || !collBankAcct) throw new Error('bank fetch failed');
  const debtBank = decodeBank(debtBankAcct.data);
  const collBank = decodeBank(collBankAcct.data);

  // marginfi v0.1.8 Bank: liquidity_vault at body offset 104 (8 disc + 104).
  const ANCHOR_DISC_LEN = 8;
  const liquidityVault = new PublicKey(
    new Uint8Array(debtBankAcct.data.buffer, debtBankAcct.data.byteOffset + ANCHOR_DISC_LEN + 104, 32),
  );
  // bank_liquidity_vault_authority = ["liquidity_vault_auth", bank].
  const [debtBankLva] = PublicKey.findProgramAddressSync(
    [Buffer.from('liquidity_vault_auth'), header.debtLendingPool.toBuffer()],
    MARGINFI_PROGRAM_ID,
  );

  const borrower = borrowerSeat.owner;
  console.log(`\nBuilding convert ix…`);
  const ix = convertP2poolToFixedInstruction({
    borrower,
    market: loan.market,
    loanSequence: loan.matchedLoanSequence,
    debtMint: header.debtMint,
    debtBank: header.debtLendingPool,
    debtLiquidityVault: liquidityVault,
    debtBankLiquidityVaultAuthority: debtBankLva,
    // Slice to the active oracle count (1 for Pyth-push / Switchboard-pull,
    // 3 for staked LST). `bank.oracleKeys` is a fixed length-5 array padded
    // with system_program for unused slots; passing them all shifts the
    // collateral_bank slot past the loader's strict offset.
    debtOracles: debtBank.oracleKeys.slice(0, oracleCountFor(debtBank.oracleSetup)),
    collateralBank: header.collateralLendingPool,
    collateralOracles: collBank.oracleKeys.slice(0, oracleCountFor(collBank.oracleSetup)),
    tokenProgram: TOKEN_PROGRAM_ID,
    marginfiGroup: header.marginfiGroup,
    marginfiProgram: MARGINFI_PROGRAM_ID,
    maxAcceptableRateBps: 5000, // generous — we just want to dry-run
    crankerRefund: loan.createdBy,
  });

  console.log(`\nIx account list (${ix.keys.length} accounts):`);
  ix.keys.forEach((k, i) => {
    console.log(
      `  [${i.toString().padStart(2, '0')}] ${k.pubkey.toBase58()}  ${k.isWritable ? 'w' : 'r'}${k.isSigner ? 's' : ''}`,
    );
  });

  const blockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
  const msg = new TransactionMessage({
    payerKey: borrower,
    recentBlockhash: blockhash,
    instructions: [ComputeBudgetProgram.setComputeUnitLimit({ units: 800_000 }), ix],
  }).compileToV0Message();
  const tx = new VersionedTransaction(msg);

  console.log(`\nSimulating…`);
  const sim = await conn.simulateTransaction(tx, { sigVerify: false, replaceRecentBlockhash: true });
  console.log(`\nresult.err: ${JSON.stringify(sim.value.err)}`);
  console.log(`unitsConsumed: ${sim.value.unitsConsumed}`);
  console.log(`logs:`);
  for (const l of sim.value.logs ?? []) console.log(`  ${l}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
