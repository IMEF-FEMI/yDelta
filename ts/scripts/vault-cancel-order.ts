/**
 * vault-cancel-order.ts — `CancelOrderForRiskProfile` (tag 13). Idempotent;
 * succeeds silently if there's nothing to cancel.
 *
 * Reads:
 *   .local/vault-cancel-order-input.json {
 *     marketLabel: string,
 *     mint: string,
 *     profileId: number
 *   }
 */
import { PublicKey } from '@solana/web3.js';

import { cancelOrderForRiskProfileInstruction } from '../src/instructions/index.js';
import {
  appendTxLog,
  loadConnection,
  loadSigner,
  log,
  readJson,
  readKeypairFromBase58,
  sendIxs,
} from './_runner.js';
import type { CuratorDump, MarketDump, ProfileDump } from './_types.js';
import { resolveCuratorForProfile } from './_types.js';

interface Input {
  marketLabel: string;
  mint: string;
  profileId: number;
}

async function main(): Promise<void> {
  const input = readJson<Input>('vault-cancel-order-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const profiles = readJson<Record<string, ProfileDump[]>>('risk-profiles.json');
  const curators = readJson<CuratorDump[]>('curators.json');

  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const curator = resolveCuratorForProfile(profiles, curators, input.mint, input.profileId);
  const curatorKp = readKeypairFromBase58(curator.secretKeyBase58);

  const conn = loadConnection();
  const feePayer = loadSigner();

  const ix = cancelOrderForRiskProfileInstruction({
    feePayer: feePayer.publicKey,
    curator: curatorKp.publicKey,
    mint: new PublicKey(input.mint),
    market: new PublicKey(market.market),
    profileId: input.profileId,
  });
  const sig = await sendIxs(conn, feePayer, [ix], [curatorKp]);
  log(`[vault-cancel-order] signature = ${sig}`);
  appendTxLog({
    script: 'vault-cancel-order',
    signatures: [sig],
    summary: {
      marketLabel: input.marketLabel,
      mint: input.mint,
      profileId: input.profileId,
      curator: curatorKp.publicKey.toBase58(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
