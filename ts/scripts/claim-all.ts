/**
 * claim-all.ts — close out every repaid/settled VAULT-lent Fixed loan on
 * a market by claiming its repayment (`ClaimRepaymentForRiskProfile`,
 * tag 20). Permissionless: drains the repaid USDC back into vault shares
 * and CLOSES the loan PDA (rent refunded to `loan.created_by`).
 *
 * Auto-discovers Fixed loan PDAs (288-byte, market@8) whose state is not
 * Active(0) — i.e. Repaid/Settled and still un-claimed — and claims each.
 * Per-loan try/catch so one odd loan doesn't block the rest. Cranks the
 * debt-bank oracle once up front (claim reads it for the lender-side
 * marginfi withdraw).
 *
 * Usage: yarn tsx ts/scripts/claim-all.ts <marketLabel>
 */
import { PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  claimRepaymentForRiskProfileInstruction,
  cuBudgetIx,
} from '../src/instructions/index.js';
import { globalVaultPda, lenderIntegrationAccountPda } from '../src/pdas.js';
import { YDELTA_PROGRAM_ID } from '../src/constants.js';
import { decodeLoanFixed, LOAN_FIXED_SIZE } from '../src/accounts/loan.js';
import { loadConnection, loadSigner, log, readJson, sendIxs } from './_runner.js';
import {
  bankLiquidityVaultAuthority,
  MARGINFI_PROGRAM_ID,
  readBankOnchain,
} from './_marginfi.js';
import { crankStaleBankOracles } from './_oracleCrank.js';
import type { MarketDump } from './_types.js';

async function main(): Promise<void> {
  const marketLabel = process.argv[2] ?? 'USDC/SOL';
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${marketLabel}`);

  const conn = loadConnection();
  const signer = loadSigner();
  const marketPk = new PublicKey(market.market);
  const debtMint = new PublicKey(market.debtMint);
  const debtBank = new PublicKey(market.debtBank);
  const debtBankState = await readBankOnchain(conn, debtBank);

  // Discover non-Active Fixed loans (Repaid/Settled, awaiting claim-close).
  const accts = await conn.getProgramAccounts(YDELTA_PROGRAM_ID, {
    filters: [
      { dataSize: LOAN_FIXED_SIZE },
      { memcmp: { offset: 8, bytes: marketPk.toBase58() } },
    ],
  });
  type Row = { seq: bigint; createdBy: PublicKey };
  const loans: Row[] = [];
  for (const { account } of accts) {
    const l = decodeLoanFixed(account.data) as {
      matchedLoanSequence: bigint;
      loanType: number;
      state: number;
      createdBy: PublicKey;
    };
    if (l.loanType !== 0) continue; // Fixed (vault-lent) only
    if (l.state === 0) continue; // skip still-Active
    loans.push({ seq: l.matchedLoanSequence, createdBy: l.createdBy });
  }
  loans.sort((a, b) => (a.seq < b.seq ? -1 : a.seq > b.seq ? 1 : 0));
  log(`[claim-all] ${marketLabel}: ${loans.length} repaid Fixed loans to claim/close`);
  if (loans.length === 0) return;

  // Crank the debt (USDC) oracle once; claim reads it for the lender-side
  // marginfi withdraw. Pyth-push stays fresh across the loop.
  await crankStaleBankOracles(conn, signer, [
    { bank: debtBankState, pythFeedIdHex: market.debtPythFeedIdHex, pythShardId: market.debtPythShardId },
  ]);

  const ok: string[] = [];
  const failed: { seq: string; err: string }[] = [];
  for (const ln of loans) {
    try {
      const ix = claimRepaymentForRiskProfileInstruction({
        payer: signer.publicKey,
        market: marketPk,
        sequence: ln.seq,
        globalVault: globalVaultPda(debtMint)[0],
        debtMint,
        debtBank,
        debtLiquidityVault: new PublicKey(market.debtLiquidityVault),
        debtBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(debtBank),
        bankOracle: debtBankState.oracleKeys[0],
        lenderMarginfiAccount: lenderIntegrationAccountPda(marketPk)[0],
        tokenProgram: TOKEN_PROGRAM_ID,
        marginfiGroup: new PublicKey(market.marginfiGroup),
        marginfiProgram: MARGINFI_PROGRAM_ID,
        crankerRefund: ln.createdBy,
      });
      const sig = await sendIxs(conn, signer, [cuBudgetIx(), ix]);
      log(`[claim-all]   seq=${ln.seq} → ${sig}`);
      ok.push(ln.seq.toString());
    } catch (e) {
      const err = (e as Error).message ?? String(e);
      log(`[claim-all]   seq=${ln.seq} FAILED: ${err.split('\n')[0]}`);
      failed.push({ seq: ln.seq.toString(), err: err.split('\n')[0] });
    }
  }
  log(`[claim-all] done — claimed ${ok.length}, failed ${failed.length}`);
  for (const f of failed) log(`  FAILED seq=${f.seq}: ${f.err}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
