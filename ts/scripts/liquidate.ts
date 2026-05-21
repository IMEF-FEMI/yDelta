/**
 * liquidate.ts — `LiquidateLoan` (tag 17). Same account ordering as
 * settle, but the on-chain gate is "LTV breach", not "past-grace".
 * Reverts with `LoanStillSolvent` if maintenance-LTV isn't exceeded.
 *
 * Reads:
 *   .local/liquidate-input.json {
 *     marketLabel: string,
 *     sequence: string | number,
 *     liquidatorDebtTokenAta: string,
 *     liquidatorCollateralTokenAta: string,
 *     repayAtomsMax: string | number,
 *     crankerRefund: string
 *   }
 */
import { PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  cuBudgetIx,
  liquidateLoanInstruction,
} from '../src/instructions/index.js';
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
  liquidatorDebtTokenAta: string;
  liquidatorCollateralTokenAta: string;
  repayAtomsMax: string | number;
  crankerRefund: string;
}

async function main(): Promise<void> {
  const input = readJson<Input>('liquidate-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const conn = loadConnection();
  const signer = loadSigner();
  const debtBank = new PublicKey(market.debtBank);
  const collateralBank = new PublicKey(market.collateralBank);
  const debtBankState = await readBankOnchain(conn, debtBank);
  const collateralBankState = await readBankOnchain(conn, collateralBank);
  const crank = await crankStaleBankOracles(conn, signer, [
    { bank: debtBankState, pythFeedIdHex: market.debtPythFeedIdHex, pythShardId: market.debtPythShardId },
    { bank: collateralBankState, pythFeedIdHex: market.collateralPythFeedIdHex, pythShardId: market.collateralPythShardId },
  ]);

  const ix = liquidateLoanInstruction({
    payer: signer.publicKey,
    market: new PublicKey(market.market),
    sequence: BigInt(input.sequence.toString()),
    debtMint: new PublicKey(market.debtMint),
    collateralMint: new PublicKey(market.collateralMint),
    liquidatorDebtToken: new PublicKey(input.liquidatorDebtTokenAta),
    liquidatorCollateralToken: new PublicKey(input.liquidatorCollateralTokenAta),
    debtBank,
    collateralBank,
    debtLiquidityVault: new PublicKey(market.debtLiquidityVault),
    collateralLiquidityVault: new PublicKey(market.collateralLiquidityVault),
    collateralBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(collateralBank),
    debtOracles: market.debtOracles.map((s) => new PublicKey(s)),
    collateralOracles: market.collateralOracles.map((s) => new PublicKey(s)),
    tokenProgram: TOKEN_PROGRAM_ID,
    marginfiGroup: new PublicKey(market.marginfiGroup),
    marginfiProgram: MARGINFI_PROGRAM_ID,
    repayAtomsMax: BigInt(input.repayAtomsMax.toString()),
    crankerRefund: new PublicKey(input.crankerRefund),
  });
  log(`[liquidate] ${market.label} seq=${input.sequence}`);
  const sig = await sendIxs(conn, signer, [cuBudgetIx(), ix]);
  log(`[liquidate] signature = ${sig}`);
  appendTxLog({
    script: 'liquidate',
    signatures: [sig],
    oracleCrank: crank.entries,
    summary: {
      marketLabel: input.marketLabel,
      sequence: input.sequence.toString(),
      repayAtomsMax: input.repayAtomsMax.toString(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
