/**
 * borrower-cancel-bid.ts — `CancelOrder` (tag 42, v1 D6). Borrower-signed
 * cancel of a resting bid, releasing its encumbered collateral. Removes
 * the market-side `RestingOrder` + the borrower's `UserOrderRef`.
 *
 * Reads:
 *   .local/borrower-cancel-bid-input.json {
 *     marketLabel: string,
 *     borrowerKeypairPath?: string,         // path; defaults to YDELTA_BORROWER signer
 *     orderSequence: string | number,       // resting-bid order seq (u64)
 *     seatIndexHint?: number | null
 *   }
 *
 * Cancel doesn't touch oracles, so no crank ixs needed.
 */
import { PublicKey } from '@solana/web3.js';

import { cancelOrderInstruction } from '../src/instructions/index.js';
import {
  appendTxLog,
  loadBorrower,
  loadConnection,
  log,
  readJson,
  sendIxs,
} from './_runner.js';
import type { MarketDump } from './_types.js';

interface Input {
  marketLabel: string;
  orderSequence: string | number;
  seatIndexHint?: number | null;
}

async function main(): Promise<void> {
  const input = readJson<Input>('borrower-cancel-bid-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const conn = loadConnection();
  const borrower = loadBorrower();

  const orderSequence = BigInt(input.orderSequence.toString());
  log(`[borrower-cancel-bid] market=${market.market} seq=${orderSequence} borrower=${borrower.publicKey.toBase58()}`);

  const ix = cancelOrderInstruction({
    payer: borrower.publicKey,
    market: new PublicKey(market.market),
    orderSequence,
    seatIndexHint: input.seatIndexHint ?? null,
  });
  const sig = await sendIxs(conn, borrower, [ix]);
  log(`[borrower-cancel-bid] signature = ${sig}`);
  appendTxLog({
    script: 'borrower-cancel-bid',
    signatures: [sig],
    summary: {
      marketLabel: input.marketLabel,
      orderSequence: orderSequence.toString(),
      borrower: borrower.publicKey.toBase58(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
