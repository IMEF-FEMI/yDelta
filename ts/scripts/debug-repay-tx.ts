/**
 * debug-repay-tx.ts — fetch a live Fixed-loan PDA, build the repay tx via
 * the SDK exactly as the UI does, and dry-run it through
 * `connection.simulateTransaction`. The sim log shows whether the SDK is
 * producing the right account list — and if it isn't, where it falls short.
 *
 * Usage:
 *   yarn tsx ts/scripts/debug-repay-tx.ts --loan <pubkey>
 *   yarn tsx ts/scripts/debug-repay-tx.ts --borrower <wallet>   # finds first open Fixed loan
 *
 * Reads from `YDELTA_RPC_URL` / `RPC_URL` env. The signer is loaded from
 * `~/.config/solana/id.json` but we never broadcast — simulation only.
 */
import 'dotenv/config';

import { readFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import {
  ComputeBudgetProgram,
  Connection,
  Keypair,
  PublicKey,
  TransactionMessage,
  VersionedTransaction,
} from '@solana/web3.js';
import bs58 from 'bs58';
import { TOKEN_PROGRAM_ID, getAssociatedTokenAddressSync } from '@solana/spl-token';

import {
  decodeLoanFixed,
  fetchMarketHeader,
  LoanType,
  LOAN_FIXED_DISCRIMINANT,
  LOAN_FIXED_SIZE,
  LoanState,
  repayInstruction,
  YDELTA_PROGRAM_ID,
} from '../src/index.js';
import { MARGINFI_PROGRAM_ID } from './_marginfi.js';

const argv = process.argv.slice(2);
const arg = (k: string): string | undefined => {
  const i = argv.indexOf(k);
  return i >= 0 ? argv[i + 1] : undefined;
};

const RPC_URL =
  process.env.YDELTA_RPC_URL ??
  process.env.RPC_URL ??
  'https://api.mainnet-beta.solana.com';

function loadDefaultSigner(): Keypair {
  const p = arg('--signer') ?? path.join(os.homedir(), '.config', 'solana', 'id.json');
  const raw = JSON.parse(readFileSync(p, 'utf-8'));
  return Keypair.fromSecretKey(Uint8Array.from(raw));
}

async function findFixedLoanFor(
  conn: Connection,
  borrower: PublicKey | null,
): Promise<PublicKey | null> {
  // Filter by discriminator only; we'll decode and pick the first Fixed-Active
  // loan whose borrower matches (or any active Fixed loan if borrower is null).
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
    } catch (e) {
      console.log(`  ${pubkey.toBase58()}  decode-error: ${(e as Error).message}`);
      continue;
    }
    console.log(
      `  ${pubkey.toBase58()}  state=${loan.state} type=${loan.loanType} createdBy=${loan.createdBy.toBase58()} outstanding=${loan.outstandingDebtAtoms}`,
    );
    if (loan.state !== LoanState.Active) continue;
    if (loan.loanType !== LoanType.Fixed) continue;
    // We don't have the borrower's wallet on the loan PDA itself — it's
    // stored on the seat indexed by borrower_seat_index. For dry-run
    // purposes the borrower filter is best-effort; without --loan/--borrower
    // we just take the first active Fixed loan.
    void borrower;
    return pubkey;
  }
  return null;
}

async function main() {
  const conn = new Connection(RPC_URL, 'confirmed');
  const signer = loadDefaultSigner();
  console.log(`RPC: ${RPC_URL}`);
  console.log(`Signer: ${signer.publicKey.toBase58()}`);

  let loanPk: PublicKey | null = null;
  const loanArg = arg('--loan');
  const borrowerArg = arg('--borrower');
  if (loanArg) {
    loanPk = new PublicKey(loanArg);
  } else {
    const borrower = borrowerArg ? new PublicKey(borrowerArg) : null;
    if (borrower) {
      console.log(`Searching for an active Fixed loan by borrower ${borrower.toBase58()}…`);
    } else {
      console.log(`No --borrower / --loan passed; picking any active Fixed loan…`);
    }
    loanPk = await findFixedLoanFor(conn, borrower);
    if (!loanPk) throw new Error('No active Fixed loan found');
  }
  console.log(`Loan: ${loanPk.toBase58()}`);

  const loanAcct = await conn.getAccountInfo(loanPk);
  if (!loanAcct) throw new Error('loan account missing');
  const loan = decodeLoanFixed(loanAcct.data);
  console.log(`  state=${loan.state} type=${loan.loanType} sequence=${loan.matchedLoanSequence}`);
  console.log(`  market=${loan.market.toBase58()}`);
  console.log(`  borrowerSeatIndex=${loan.borrowerSeatIndex}`);
  console.log(`  lenderKind=${loan.lenderKind} lenderGlobalVault=${loan.lenderGlobalVault.toBase58()}`);
  console.log(`  outstanding=${loan.outstandingDebtAtoms} principal=${loan.principalDebtAtoms}`);

  // Look up borrower from the market's claimed-seats tree (the loan PDA
  // only carries the seat index, not the wallet pubkey).
  const marketAcct = await conn.getAccountInfo(loan.market);
  if (!marketAcct) throw new Error('market account missing');
  const { lookupClaimedSeatAt } = await import('../src/index.js');
  const borrowerSeat = lookupClaimedSeatAt(marketAcct.data, loan.borrowerSeatIndex);
  if (!borrowerSeat) throw new Error('borrower seat lookup failed');
  console.log(`  borrower (from seat)=${borrowerSeat.owner.toBase58()}`);

  // Read market header to get bank + group + mint.
  const header = await fetchMarketHeader(conn, loan.market);
  if (!header) throw new Error('market header missing');
  console.log(`  debt_mint=${header.debtMint.toBase58()} debt_bank=${header.debtLendingPool.toBase58()}`);
  console.log(`  collateral_bank=${header.collateralLendingPool.toBase58()}`);
  console.log(`  marginfi_group=${header.marginfiGroup.toBase58()}`);

  // Read debt bank's liquidity_vault (body offset 104 in marginfi v0.1.8 bank).
  const bankAcct = await conn.getAccountInfo(header.debtLendingPool);
  if (!bankAcct) throw new Error('debt bank missing');
  const ANCHOR_DISC_LEN = 8;
  const bankView = new DataView(bankAcct.data.buffer, bankAcct.data.byteOffset, bankAcct.data.byteLength);
  const liquidityVault = new PublicKey(
    new Uint8Array(bankAcct.data.buffer, bankAcct.data.byteOffset + ANCHOR_DISC_LEN + 104, 32),
  );
  console.log(`  debt_liquidity_vault=${liquidityVault.toBase58()}`);
  void bankView;

  const borrower = borrowerSeat.owner;
  const borrowerToken = getAssociatedTokenAddressSync(header.debtMint, borrower, true);
  console.log(`  borrower_token (ATA)=${borrowerToken.toBase58()}`);

  const isFixed = loan.loanType === LoanType.Fixed;
  console.log(`\nBuilding repay ix with globalVault=${isFixed ? loan.lenderGlobalVault.toBase58() : 'null'}`);
  const ix = repayInstruction({
    borrower,
    market: loan.market,
    sequence: loan.matchedLoanSequence,
    debtMint: header.debtMint,
    borrowerToken,
    tokenProgram: TOKEN_PROGRAM_ID,
    marginfiGroup: header.marginfiGroup,
    debtBank: header.debtLendingPool,
    debtLiquidityVault: liquidityVault,
    collateralBank: header.collateralLendingPool,
    marginfiProgram: MARGINFI_PROGRAM_ID,
    repayAtoms: 0n,
    fullRepay: true,
    crankerRefund: loan.createdBy,
    globalVault: isFixed ? loan.lenderGlobalVault : null,
  });

  console.log(`\nIx account list (${ix.keys.length} accounts):`);
  ix.keys.forEach((k, i) => {
    console.log(`  [${i.toString().padStart(2, '0')}] ${k.pubkey.toBase58()}  ${k.isWritable ? 'w' : 'r'}${k.isSigner ? 's' : ''}`);
  });

  const blockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
  const msg = new TransactionMessage({
    payerKey: borrower,
    recentBlockhash: blockhash,
    instructions: [
      ComputeBudgetProgram.setComputeUnitLimit({ units: 600_000 }),
      ix,
    ],
  }).compileToV0Message();
  const tx = new VersionedTransaction(msg);
  // No sign — we don't broadcast, just simulate. Solana sim will accept
  // an unsigned tx for read-only sim if we pass sigVerify=false.

  console.log(`\nSimulating…`);
  const sim = await conn.simulateTransaction(tx, {
    sigVerify: false,
    replaceRecentBlockhash: true,
  });
  console.log(`\nresult.err: ${JSON.stringify(sim.value.err)}`);
  console.log(`unitsConsumed: ${sim.value.unitsConsumed}`);
  console.log(`logs:`);
  for (const l of sim.value.logs ?? []) {
    console.log(`  ${l}`);
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
