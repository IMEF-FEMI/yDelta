/**
 * claim-repayment.ts — `ClaimRepaymentForSubVault` (tag 20).
 * Permissionless cranker. Sweeps a sub-vault's `pendingClaimAtoms` from
 * the market's lender marginfi account back into the vault's own
 * integration account.
 *
 * Reads:
 *   .local/claim-repayment-input.json {
 *     marketLabel: string,
 *     subVaultId: number
 *   }
 */
import { PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  claimRepaymentForSubVaultInstruction,
  cuBudgetIx,
} from '../src/instructions/index.js';
import {
  globalVaultPda,
  lenderIntegrationAccountPda,
} from '../src/pdas.js';
import {
  appendTxLog,
  loadConnection,
  loadSigner,
  log,
  readJson,
  sendIxs,
} from './_runner.js';
import {
  bankLiquidityVaultAuthority,
  MARGINFI_PROGRAM_ID,
  readBankOnchain,
} from './_marginfi.js';
import { crankStaleBankOracles } from './_oracleCrank.js';
import type { MarketDump } from './_types.js';

interface Input {
  marketLabel: string;
  subVaultId: number;
}

async function main(): Promise<void> {
  const input = readJson<Input>('claim-repayment-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const conn = loadConnection();
  const signer = loadSigner();
  const debtMint = new PublicKey(market.debtMint);
  const debtBank = new PublicKey(market.debtBank);
  const debtBankState = await readBankOnchain(conn, debtBank);
  const crank = await crankStaleBankOracles(conn, signer, [
    { bank: debtBankState, pythFeedIdHex: market.debtPythFeedIdHex, pythShardId: market.debtPythShardId },
  ]);

  const ix = claimRepaymentForSubVaultInstruction({
    payer: signer.publicKey,
    market: new PublicKey(market.market),
    subVaultId: input.subVaultId,
    // The vault PDA is bank-keyed (`[b"vault", bank]`).
    globalVault: globalVaultPda(debtBank)[0],
    debtMint,
    debtBank,
    debtLiquidityVault: new PublicKey(market.debtLiquidityVault),
    debtBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(debtBank),
    // Only the bank's primary oracle is loaded by the vault-settle path.
    bankOracles: [debtBankState.oracleKeys[0]],
    lenderMarginfiAccount: lenderIntegrationAccountPda(new PublicKey(market.market))[0],
    tokenProgram: TOKEN_PROGRAM_ID,
    marginfiGroup: new PublicKey(market.marginfiGroup),
    marginfiProgram: MARGINFI_PROGRAM_ID,
  });
  log(`[claim-repayment] ${market.label} subVaultId=${input.subVaultId}`);
  const sig = await sendIxs(conn, signer, [cuBudgetIx(), ix]);
  log(`[claim-repayment] signature = ${sig}`);
  appendTxLog({
    script: 'claim-repayment',
    signatures: [sig],
    oracleCrank: crank.entries,
    summary: {
      marketLabel: input.marketLabel,
      subVaultId: input.subVaultId,
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
