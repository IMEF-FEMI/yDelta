/**
 * crank-matched-loan.ts — `ProcessMatchedLoan` (tag 5). Permissionless;
 * the cranker pays the loan PDA rent and is reimbursed at loan close.
 *
 * Reads:
 *   .local/crank-matched-loan-input.json {
 *     marketLabel: string,
 *     sequence: string | number,
 *     matchedLoanIndexHint?: number | null,
 *     vaultSettle?: boolean        // true if the matched lender is a vault profile
 *   }
 */
import { PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  cuBudgetIx,
  processMatchedLoanInstruction,
} from '../src/instructions/index.js';
import {
  borrowerIntegrationAccountPda,
  globalVaultIntegrationAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
  lenderIntegrationAccountPda,
  marketSignerPda,
  marketTokenVaultPda,
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
  MARGINFI_PROGRAM_ID,
  readBankOnchain,
} from './_marginfi.js';
import { crankStaleBankOracles } from './_oracleCrank.js';
import type { MarketDump } from './_types.js';

interface Input {
  marketLabel: string;
  sequence: string | number;
  matchedLoanIndexHint?: number | null;
  vaultSettle?: boolean;
}

async function main(): Promise<void> {
  const input = readJson<Input>('crank-matched-loan-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const conn = loadConnection();
  const signer = loadSigner();
  const marketPk = new PublicKey(market.market);
  const debtMint = new PublicKey(market.debtMint);
  const debtBank = new PublicKey(market.debtBank);

  const debtBankState = await readBankOnchain(conn, debtBank);
  const crank = await crankStaleBankOracles(conn, signer, [
    { bank: debtBankState, pythFeedIdHex: market.debtPythFeedIdHex, pythShardId: market.debtPythShardId },
  ]);

  // `markets.json` was bumped to remember the vaultSettle bit through to
  // the cranker step; the alternative is to decode the matched-loan
  // entry's `flags` here. We accept either: explicit flag wins.
  //
  // The GlobalVault PDA is bank-keyed in v1 (`[b"vault", bank]`), so we
  // derive it from `debtBank` (the market's debt lending pool), NOT the mint.
  const vault = globalVaultPda(debtBank)[0];
  const ix = processMatchedLoanInstruction({
    payer: signer.publicKey,
    market: marketPk,
    debtBank,
    marginfiProgram: MARGINFI_PROGRAM_ID,
    sequence: BigInt(input.sequence.toString()),
    matchedLoanIndexHint: input.matchedLoanIndexHint ?? null,
    vaultSettle: input.vaultSettle
      ? {
          globalVault: vault,
          globalVaultSigner: globalVaultSignerPda(vault)[0],
          globalVaultStaging: globalVaultStagingPda(vault)[0],
          globalVaultIntegrationAccount: globalVaultIntegrationAccountPda(vault)[0],
          marketDebtVault: marketTokenVaultPda(marketPk, debtMint)[0],
          marketLenderIntegrationAccount: lenderIntegrationAccountPda(marketPk)[0],
          marketSigner: marketSignerPda(marketPk)[0],
          debtLiquidityVault: new PublicKey(market.debtLiquidityVault),
          debtBankLiquidityVaultAuthority: new PublicKey(
            market.debtBankLiquidityVaultAuthority,
          ),
          debtOracles: market.debtOracles.map((s) => new PublicKey(s)),
          debtMint,
          tokenProgram: TOKEN_PROGRAM_ID,
          marginfiGroup: new PublicKey(market.marginfiGroup),
          marginfiProgram: MARGINFI_PROGRAM_ID,
        }
      : undefined,
  });

  log(`[crank-matched-loan] ${market.label} seq=${input.sequence} vaultSettle=${input.vaultSettle ?? false}`);
  const sig = await sendIxs(conn, signer, [cuBudgetIx(), ix]);
  log(`[crank-matched-loan] signature = ${sig}`);
  appendTxLog({
    script: 'crank-matched-loan',
    signatures: [sig],
    oracleCrank: crank.entries,
    summary: {
      marketLabel: input.marketLabel,
      sequence: input.sequence.toString(),
      vaultSettle: input.vaultSettle ?? false,
    },
  });

  // Reserved for downstream tools that wire borrower seat ops alongside
  // the cranker — referenced so the import isn't left dangling.
  void borrowerIntegrationAccountPda;
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
