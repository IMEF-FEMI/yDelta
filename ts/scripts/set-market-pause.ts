/**
 * set-market-pause.ts — `SetMarketPause` (tag 27).
 *
 * Reads:
 *   .local/set-market-pause-input.json {
 *     marketLabel: string,
 *     paused: boolean
 *   }
 */
import { PublicKey } from '@solana/web3.js';

import { setMarketPauseInstruction } from '../src/instructions/index.js';
import {
  appendTxLog,
  loadConnection,
  loadSigner,
  log,
  readJson,
  sendIxs,
} from './_runner.js';
import type { MarketDump } from './_types.js';

interface Input {
  marketLabel: string;
  paused: boolean;
}

async function main(): Promise<void> {
  const input = readJson<Input>('set-market-pause-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const conn = loadConnection();
  const signer = loadSigner();

  const ix = setMarketPauseInstruction({
    admin: signer.publicKey,
    market: new PublicKey(market.market),
    paused: input.paused,
  });
  const sig = await sendIxs(conn, signer, [ix]);
  log(`[set-market-pause] ${market.label} paused=${input.paused} signature = ${sig}`);
  appendTxLog({
    script: 'set-market-pause',
    signatures: [sig],
    summary: { marketLabel: input.marketLabel, paused: input.paused },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
