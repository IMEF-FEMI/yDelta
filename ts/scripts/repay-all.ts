/**
 * repay-all.ts — repay EVERY active loan the borrower holds on a market,
 * one `Repay` (tag 6) tx per loan, `full_repay = true`.
 *
 * Auto-discovers the borrower's open loan PDAs via getProgramAccounts
 * (288-byte LoanFixed, market at offset 8), skips already-settled ones,
 * and repays each with `crankerRefund = loan.created_by` (the program
 * validates that equality on close). Repay touches no oracles, so no
 * crank ixs are bundled.
 *
 * Reads .local/markets.json for the market params and the
 * YDELTA_BORROWER_KEYPAIR_B58 signer. No per-loan input file — the loans
 * are read live from chain.
 *
 * Usage: yarn tsx ts/scripts/repay-all.ts <marketLabel>
 */
import { PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import { repayInstruction } from '../src/instructions/index.js';
import { YDELTA_PROGRAM_ID } from '../src/constants.js';
import { decodeLoanFixed, LOAN_FIXED_SIZE } from '../src/accounts/loan.js';
import {
  ataFor,
  createAtaIdempotentIx,
  loadBorrower,
  loadConnection,
  log,
  readJson,
  sendIxs,
} from './_runner.js';
import { MARGINFI_PROGRAM_ID } from './_marginfi.js';
import type { MarketDump } from './_types.js';

async function main(): Promise<void> {
  const marketLabel = process.argv[2] ?? 'USDC/SOL';
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${marketLabel}`);

  const conn = loadConnection();
  const borrower = loadBorrower();
  const marketPk = new PublicKey(market.market);
  const debtMint = new PublicKey(market.debtMint);

  // Discover the borrower's open loans on this market.
  const accts = await conn.getProgramAccounts(YDELTA_PROGRAM_ID, {
    filters: [
      { dataSize: LOAN_FIXED_SIZE },
      { memcmp: { offset: 8, bytes: marketPk.toBase58() } },
    ],
  });
  type Row = { seq: bigint; type: string; createdBy: PublicKey; globalVault: PublicKey | null };
  const loans: Row[] = [];
  for (const { account } of accts) {
    const l = decodeLoanFixed(account.data) as {
      matchedLoanSequence: bigint;
      loanType: number;
      state: number;
      createdBy: PublicKey;
      lenderGlobalVault: PublicKey;
    };
    if (l.state !== 0) continue; // skip settled/repaid
    loans.push({
      seq: l.matchedLoanSequence,
      type: l.loanType === 1 ? 'P2Pool' : 'Fixed',
      createdBy: l.createdBy,
      // Fixed (vault-lent) full repay credits the lender on the vault-owned
      // seat, so the program REQUIRES the lender global vault as a trailing
      // account; P2Pool loans omit it.
      globalVault: l.loanType === 1 ? null : l.lenderGlobalVault,
    });
  }
  // Repay in sequence order; P2Pool loans each close on their own slice,
  // the last one zeroing any shared-liability dust via repay_all.
  loans.sort((a, b) => (a.seq < b.seq ? -1 : a.seq > b.seq ? 1 : 0));

  log(`[repay-all] ${marketLabel}: ${loans.length} active loans for ${borrower.publicKey.toBase58()}`);
  if (loans.length === 0) {
    log('[repay-all] nothing to repay');
    return;
  }

  const results: { seq: string; type: string; sig: string }[] = [];
  for (const ln of loans) {
    log(`[repay-all] repaying seq=${ln.seq} (${ln.type}) refund=${ln.createdBy.toBase58()}`);
    const ix = repayInstruction({
      borrower: borrower.publicKey,
      market: marketPk,
      sequence: ln.seq,
      debtMint,
      borrowerToken: ataFor(borrower.publicKey, debtMint),
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiGroup: new PublicKey(market.marginfiGroup),
      debtBank: new PublicKey(market.debtBank),
      debtLiquidityVault: new PublicKey(market.debtLiquidityVault),
      collateralBank: new PublicKey(market.collateralBank),
      marginfiProgram: MARGINFI_PROGRAM_ID,
      repayAtoms: 0n,
      fullRepay: true,
      borrowerSeatIndexHint: null,
      crankerRefund: ln.createdBy,
      globalVault: ln.globalVault,
    });
    const sig = await sendIxs(conn, borrower, [
      createAtaIdempotentIx(borrower.publicKey, borrower.publicKey, debtMint),
      ix,
    ]);
    log(`[repay-all]   seq=${ln.seq} → ${sig}`);
    results.push({ seq: ln.seq.toString(), type: ln.type, sig });
  }

  log(`[repay-all] done — ${results.length} loans repaid`);
  for (const r of results) log(`  seq=${r.seq} ${r.type} ${r.sig}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
