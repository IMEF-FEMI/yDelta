/**
 * vault-place-ask.ts — `PlaceOrderForRiskProfile` (tag 12). Curator
 * publishes (or repositions) a vault ask at a given rate/term.
 *
 * Reads:
 *   .local/vault-place-ask-input.json {
 *     marketLabel: string,                   // .local/markets.json key
 *     mint: string,                          // debt mint (must equal market.debtMint)
 *     profileId: number,
 *     rateBps: number,
 *     termSeconds: number,
 *     flags?: number
 *   }
 *   .local/markets.json
 *   .local/risk-profiles.json
 *   .local/curators.json
 *
 * The curator key is loaded from `curators.json` via the profile's
 * `curatorLabel`. Fee payer = deployer (signer); curator-signs only.
 */
import { PublicKey } from '@solana/web3.js';

import { placeOrderForRiskProfileInstruction } from '../src/instructions/index.js';
import {
  appendTxLog,
  loadConnection,
  loadSigner,
  log,
  readJson,
  readKeypairFromBase58,
  sendIxs,
} from './_runner.js';
import type { MarketDump, ProfileDump, CuratorDump } from './_types.js';
import { resolveCuratorForProfile } from './_types.js';

interface Input {
  marketLabel: string;
  mint: string;
  profileId: number;
  rateBps: number;
  termSeconds: number;
  flags?: number;
}

async function main(): Promise<void> {
  const input = readJson<Input>('vault-place-ask-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const profiles = readJson<Record<string, ProfileDump[]>>('risk-profiles.json');
  const curators = readJson<CuratorDump[]>('curators.json');

  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);
  if (market.debtMint !== input.mint) {
    throw new Error(
      `market ${input.marketLabel} debtMint ${market.debtMint} ≠ input.mint ${input.mint}`,
    );
  }
  const curator = resolveCuratorForProfile(profiles, curators, input.mint, input.profileId);
  const curatorKp = readKeypairFromBase58(curator.secretKeyBase58);

  const conn = loadConnection();
  const feePayer = loadSigner();

  const ix = placeOrderForRiskProfileInstruction({
    feePayer: feePayer.publicKey,
    curator: curatorKp.publicKey,
    mint: new PublicKey(input.mint),
    market: new PublicKey(market.market),
    profileId: input.profileId,
    rateBps: input.rateBps,
    termSeconds: input.termSeconds,
    flags: input.flags,
  });
  const sig = await sendIxs(conn, feePayer, [ix], [curatorKp]);
  log(`[vault-place-ask] signature = ${sig}`);
  appendTxLog({
    script: 'vault-place-ask',
    signatures: [sig],
    summary: {
      marketLabel: input.marketLabel,
      mint: input.mint,
      profileId: input.profileId,
      rateBps: input.rateBps,
      termSeconds: input.termSeconds,
      curator: curatorKp.publicKey.toBase58(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
