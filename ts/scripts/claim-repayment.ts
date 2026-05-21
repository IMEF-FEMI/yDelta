/**
 * claim-repayment.ts — `ClaimRepaymentForRiskProfile` (tag 20).
 * Permissionless cranker. Drains a settled vault loan back into vault
 * shares and closes the loan PDA.
 *
 * Reads:
 *   .local/claim-repayment-input.json {
 *     marketLabel: string,
 *     sequence: string | number,
 *     crankerRefund: string                  // who paid the loan rent at create
 *   }
 */
import { PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  claimRepaymentForRiskProfileInstruction,
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
  sequence: string | number;
  crankerRefund: string;
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

  const ix = claimRepaymentForRiskProfileInstruction({
    payer: signer.publicKey,
    market: new PublicKey(market.market),
    sequence: BigInt(input.sequence.toString()),
    globalVault: globalVaultPda(debtMint)[0],
    debtMint,
    debtBank,
    debtLiquidityVault: new PublicKey(market.debtLiquidityVault),
    debtBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(debtBank),
    bankOracle: debtBankState.oracleKeys[0],
    lenderMarginfiAccount: lenderIntegrationAccountPda(new PublicKey(market.market))[0],
    tokenProgram: TOKEN_PROGRAM_ID,
    marginfiGroup: new PublicKey(market.marginfiGroup),
    marginfiProgram: MARGINFI_PROGRAM_ID,
    crankerRefund: new PublicKey(input.crankerRefund),
  });
  log(`[claim-repayment] ${market.label} seq=${input.sequence}`);
  const sig = await sendIxs(conn, signer, [cuBudgetIx(), ix]);
  log(`[claim-repayment] signature = ${sig}`);
  appendTxLog({
    script: 'claim-repayment',
    signatures: [sig],
    oracleCrank: crank.entries,
    summary: {
      marketLabel: input.marketLabel,
      sequence: input.sequence.toString(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
