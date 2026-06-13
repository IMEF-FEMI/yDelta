/**
 * claim-all.ts — sweep repaid/settled VAULT-lent capital back into the
 * vault for a market via `ClaimRepaymentForSubVault` (tag 20).
 *
 * v1 semantics: the claim is a per-SUB-VAULT sweeper (not per-loan). It
 * drains a sub-vault's `pending_claim_atoms` from the per-market lender
 * marginfi account into the vault's integration account. This script
 * discovers repaid Fixed loans on the market, collects the distinct
 * `lenderSubVaultId`s that have un-swept repayments, and sweeps each once.
 * Loan PDAs are closed by `repay` / `settle` / `liquidate`, not here.
 *
 * Usage: yarn tsx ts/scripts/claim-all.ts <marketLabel>
 */
import { PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  claimRepaymentForSubVaultInstruction,
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

  // Discover non-Active Fixed loans (Repaid/Settled) and collect the
  // distinct vault sub-vaults that lent them — each gets one sweep.
  const accts = await conn.getProgramAccounts(YDELTA_PROGRAM_ID, {
    filters: [
      { dataSize: LOAN_FIXED_SIZE },
      { memcmp: { offset: 8, bytes: marketPk.toBase58() } },
    ],
  });
  const subVaultIds = new Set<number>();
  for (const { account } of accts) {
    const l = decodeLoanFixed(account.data) as {
      loanType: number;
      state: number;
      lenderSubVaultId: number;
    };
    if (l.loanType !== 0) continue; // Fixed (vault-lent) only
    if (l.state === 0) continue; // skip still-Active
    if (l.lenderSubVaultId !== 0) subVaultIds.add(l.lenderSubVaultId);
  }
  const ids = [...subVaultIds].sort((a, b) => a - b);
  log(`[claim-all] ${marketLabel}: ${ids.length} sub-vault(s) with repaid Fixed loans to sweep`);
  if (ids.length === 0) return;

  // Crank the debt (USDC) oracle once; the sweep reads it for the
  // lender-side marginfi withdraw. Pyth-push stays fresh across the loop.
  await crankStaleBankOracles(conn, signer, [
    { bank: debtBankState, pythFeedIdHex: market.debtPythFeedIdHex, pythShardId: market.debtPythShardId },
  ]);

  const ok: number[] = [];
  const failed: { subVaultId: number; err: string }[] = [];
  for (const subVaultId of ids) {
    try {
      const ix = claimRepaymentForSubVaultInstruction({
        payer: signer.publicKey,
        market: marketPk,
        subVaultId,
        globalVault: globalVaultPda(debtBank)[0],
        debtMint,
        debtBank,
        debtLiquidityVault: new PublicKey(market.debtLiquidityVault),
        debtBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(debtBank),
        bankOracles: debtBankState.oracleKeys,
        lenderMarginfiAccount: lenderIntegrationAccountPda(marketPk)[0],
        tokenProgram: TOKEN_PROGRAM_ID,
        marginfiGroup: new PublicKey(market.marginfiGroup),
        marginfiProgram: MARGINFI_PROGRAM_ID,
      });
      const sig = await sendIxs(conn, signer, [cuBudgetIx(), ix]);
      log(`[claim-all]   subVaultId=${subVaultId} → ${sig}`);
      ok.push(subVaultId);
    } catch (e) {
      const err = (e as Error).message ?? String(e);
      log(`[claim-all]   subVaultId=${subVaultId} FAILED: ${err.split('\n')[0]}`);
      failed.push({ subVaultId, err: err.split('\n')[0] });
    }
  }
  log(`[claim-all] done — swept ${ok.length}, failed ${failed.length}`);
  for (const f of failed) log(`  FAILED subVaultId=${f.subVaultId}: ${f.err}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
