/**
 * settle.ts — `SettleMaturedLoan` (tag 16). Past-grace; keeper drains
 * collateral, repays lender, returns surplus to the borrower seat.
 *
 * Reads:
 *   .local/settle-input.json {
 *     marketLabel: string,
 *     sequence: string | number,
 *     liquidatorDebtTokenAta: string,
 *     liquidatorCollateralTokenAta: string,
 *     repayAtomsMax: string | number,         // 0 or >= outstanding = full repay
 *     crankerRefund: string
 *   }
 */
import { PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  cuBudgetIx,
  settleMaturedLoanInstruction,
} from '../src/instructions/index.js';
import { decodeLoanFixed, LoanType, loanPda } from '../src/index.js';
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
  const input = readJson<Input>('settle-input.json');
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

  // The full-settle close credits the lender on the vault-owned seat, so a
  // Fixed (vault-lent) loan REQUIRES its lender global vault as a trailing
  // account; P2Pool loans omit it. Decode the loan to route correctly.
  const marketPk = new PublicKey(market.market);
  const sequence = BigInt(input.sequence.toString());
  const [loanAddr] = loanPda(marketPk, sequence);
  const loanAcct = await conn.getAccountInfo(loanAddr);
  if (!loanAcct) throw new Error(`loan PDA ${loanAddr.toBase58()} not found for seq ${sequence}`);
  const loan = decodeLoanFixed(loanAcct.data);
  const globalVault =
    loan.loanType === LoanType.Fixed ? loan.lenderGlobalVault : undefined;

  const ix = settleMaturedLoanInstruction({
    payer: signer.publicKey,
    market: marketPk,
    sequence,
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
    globalVault,
  });
  log(`[settle] ${market.label} seq=${input.sequence}`);
  const sig = await sendIxs(conn, signer, [cuBudgetIx(), ix]);
  log(`[settle] signature = ${sig}`);
  appendTxLog({
    script: 'settle',
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
