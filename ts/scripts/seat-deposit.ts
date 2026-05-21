/**
 * seat-deposit.ts — `Deposit` (tag 2). Trader deposits atoms (debt OR
 * collateral, side picked by `mint`) into their market seat. Auto-claims
 * the seat if the trader has never touched this market.
 *
 * Reads:
 *   .local/seat-deposit-input.json {
 *     marketLabel: string,
 *     traderKeypairPath?: string,
 *     traderTokenAta: string,
 *     mint: string,                          // debt or collateral mint
 *     amountAtoms: string | number,
 *     traderIndexHint?: number | null
 *   }
 */
import { PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  claimSeatIfNeededInstructions,
  depositInstruction,
} from '../src/instructions/index.js';
import {
  appendTxLog,
  ataFor,
  createAtaIdempotentIx,
  loadBorrower,
  loadConnection,
  log,
  readJson,
  sendIxs,
  wrapSolIxs,
} from './_runner.js';
import { MARGINFI_PROGRAM_ID } from './_marginfi.js';
import type { MarketDump } from './_types.js';

interface Input {
  marketLabel: string;
  mint: string;
  amountAtoms: string | number;
  traderIndexHint?: number | null;
}

async function main(): Promise<void> {
  const input = readJson<Input>('seat-deposit-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const conn = loadConnection();
  const trader = loadBorrower();

  const sideMint = new PublicKey(input.mint);
  const debtMint = new PublicKey(market.debtMint);
  const isDebt = sideMint.equals(debtMint);
  if (!isDebt && !sideMint.equals(new PublicKey(market.collateralMint))) {
    throw new Error(
      `mint ${input.mint} matches neither debt (${market.debtMint}) nor collateral ` +
      `(${market.collateralMint}) of ${market.label}`,
    );
  }

  const bank = new PublicKey(isDebt ? market.debtBank : market.collateralBank);
  const liquidityVault = new PublicKey(
    isDebt ? market.debtLiquidityVault : market.collateralLiquidityVault,
  );
  const amount = BigInt(input.amountAtoms.toString());
  const traderToken = ataFor(trader.publicKey, sideMint);
  log(`[seat-deposit] ${market.label} side=${isDebt ? 'debt' : 'collateral'} amount=${amount} trader=${trader.publicKey.toBase58()}`);

  const marketPk = new PublicKey(market.market);
  const seatIxs = await claimSeatIfNeededInstructions({
    market: marketPk,
    payer: trader.publicKey,
    connection: conn,
  });

  const ix = depositInstruction({
    payer: trader.publicKey,
    market: marketPk,
    mint: sideMint,
    debtMint,
    traderToken,
    tokenProgram: TOKEN_PROGRAM_ID,
    marginfiGroup: new PublicKey(market.marginfiGroup),
    bank,
    liquidityVault,
    marginfiProgram: MARGINFI_PROGRAM_ID,
    amountAtoms: amount,
    traderIndexHint: input.traderIndexHint ?? null,
  });
  // Ensure the source ATA exists, and wrap native SOL into it when the side
  // mint is wSOL (so the borrower can deposit SOL collateral directly). Then
  // claim the seat if needed, then deposit — all atomic.
  const sig = await sendIxs(conn, trader, [
    createAtaIdempotentIx(trader.publicKey, trader.publicKey, sideMint),
    ...wrapSolIxs(trader.publicKey, sideMint, amount),
    ...seatIxs,
    ix,
  ]);
  log(`[seat-deposit] signature = ${sig}`);
  appendTxLog({
    script: 'seat-deposit',
    signatures: [sig],
    summary: {
      marketLabel: input.marketLabel,
      side: isDebt ? 'debt' : 'collateral',
      amountAtoms: amount.toString(),
      trader: trader.publicKey.toBase58(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
