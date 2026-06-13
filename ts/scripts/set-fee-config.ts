/**
 * set-fee-config.ts — `SetFeeConfig` (tag 18). Per-field optional.
 *
 * Reads:
 *   .local/set-fee-config-input.json {
 *     marketLabel: string,
 *     protocolFeeBpsFloor?: number,
 *     originationBps?: number,
 *     curatorSplitBps?: number,
 *     liquidationKeeperBps?: number,
 *     liquidationProtocolBps?: number,
 *     gracePeriodSeconds?: number
 *   }
 *
 * v1: `curatorFeeBps` is no longer a market FeeConfig field (it's per
 * sub-vault), and `ltvBufferBps` was removed entirely.
 */
import { PublicKey } from '@solana/web3.js';

import { setFeeConfigInstruction } from '../src/instructions/index.js';
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
  protocolFeeBpsFloor?: number;
  originationBps?: number;
  curatorSplitBps?: number;
  liquidationKeeperBps?: number;
  liquidationProtocolBps?: number;
  gracePeriodSeconds?: number;
}

async function main(): Promise<void> {
  const input = readJson<Input>('set-fee-config-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const conn = loadConnection();
  const signer = loadSigner();

  const ix = setFeeConfigInstruction({
    admin: signer.publicKey,
    market: new PublicKey(market.market),
    protocolFeeBpsFloor: input.protocolFeeBpsFloor,
    originationBps: input.originationBps,
    curatorSplitBps: input.curatorSplitBps,
    liquidationKeeperBps: input.liquidationKeeperBps,
    liquidationProtocolBps: input.liquidationProtocolBps,
    gracePeriodSeconds: input.gracePeriodSeconds,
  });
  const sig = await sendIxs(conn, signer, [ix]);
  log(`[set-fee-config] ${market.label} signature = ${sig}`);
  appendTxLog({
    script: 'set-fee-config',
    signatures: [sig],
    summary: { ...input },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
