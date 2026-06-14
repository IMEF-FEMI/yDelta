/**
 * borrower-update-bid.ts — `UpdateOrder` (tag 44). Borrower-signed
 * cancel-and-replace of a resting bid: the rate / term / expiry change
 * while principal and collateral are untouched. The order sequence is
 * renewed (back of price-time priority).
 *
 * Reads:
 *   .local/borrower-update-bid-input.json {
 *     marketLabel: string,
 *     borrowerKeypairPath?: string,         // path; defaults to YDELTA_BORROWER signer
 *     orderSequence: string | number,       // resting-bid order seq (u64)
 *     newRateBps: number,
 *     newTermSeconds: number,
 *     newLastValidUnixTs: string | number,  // i64; rested-bid expiry (0 = never)
 *     newLtvBufferBps?: number,             // REPLACES the bid's LTV buffer; omit/0 clears it
 *     seatIndexHint?: number | null
 *   }
 *
 * Update doesn't touch oracles (rate/term/expiry are a pure book mutation —
 * the encumbered collateral is unchanged), so no crank ixs needed.
 */
import { PublicKey } from '@solana/web3.js';

import { updateOrderInstruction } from '../src/instructions/index.js';
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
  newRateBps: number;
  newTermSeconds: number;
  newLastValidUnixTs: string | number;
  /** Optional borrower LTV buffer in bps. Note: UpdateOrder REPLACES the
   *  buffer, so omitting this (or 0) clears any buffer the bid had — pass
   *  the intended value to preserve or change it. */
  newLtvBufferBps?: number;
  seatIndexHint?: number | null;
}

async function main(): Promise<void> {
  const input = readJson<Input>('borrower-update-bid-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const conn = loadConnection();
  const borrower = loadBorrower();

  const orderSequence = BigInt(input.orderSequence.toString());
  const newLastValidUnixTs = BigInt(input.newLastValidUnixTs.toString());
  log(`[borrower-update-bid] market=${market.market} seq=${orderSequence} borrower=${borrower.publicKey.toBase58()}`);
  log(`[borrower-update-bid] newRate=${input.newRateBps}bps newTerm=${input.newTermSeconds}s newLastValidUnixTs=${newLastValidUnixTs}`);

  const ix = updateOrderInstruction({
    payer: borrower.publicKey,
    market: new PublicKey(market.market),
    orderSequence,
    newRateBps: input.newRateBps,
    newTermSeconds: input.newTermSeconds,
    newLastValidUnixTs,
    newLtvBufferBps: input.newLtvBufferBps,
    seatIndexHint: input.seatIndexHint ?? null,
  });
  const sig = await sendIxs(conn, borrower, [ix]);
  log(`[borrower-update-bid] signature = ${sig}`);
  appendTxLog({
    script: 'borrower-update-bid',
    signatures: [sig],
    summary: {
      marketLabel: input.marketLabel,
      orderSequence: orderSequence.toString(),
      newRateBps: input.newRateBps,
      newTermSeconds: input.newTermSeconds,
      newLastValidUnixTs: newLastValidUnixTs.toString(),
      borrower: borrower.publicKey.toBase58(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
