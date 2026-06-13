/**
 * vault-cancel-order.ts — `CancelOrderForSubVault` (tag 13). Idempotent;
 * succeeds silently if there's nothing to cancel.
 *
 * Reads:
 *   .local/vault-cancel-order-input.json {
 *     marketLabel: string,
 *     mint: string,
 *     subVaultId: number
 *   }
 */
import { PublicKey } from '@solana/web3.js';

import { cancelOrderForSubVaultInstruction } from '../src/instructions/index.js';
import {
  appendTxLog,
  loadConnection,
  loadSigner,
  log,
  readJson,
  readKeypairFromBase58,
  sendIxs,
} from './_runner.js';
import type { CuratorDump, MarketDump, SubVaultDump } from './_types.js';
import { resolveCuratorForSubVault } from './_types.js';

interface Input {
  marketLabel: string;
  mint: string;
  subVaultId: number;
}

async function main(): Promise<void> {
  const input = readJson<Input>('vault-cancel-order-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const subVaults = readJson<Record<string, SubVaultDump[]>>('risk-profiles.json');
  const curators = readJson<CuratorDump[]>('curators.json');

  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);

  const curator = resolveCuratorForSubVault(subVaults, curators, input.mint, input.subVaultId);
  const curatorKp = readKeypairFromBase58(curator.secretKeyBase58);

  const conn = loadConnection();
  const feePayer = loadSigner();

  const ix = cancelOrderForSubVaultInstruction({
    feePayer: feePayer.publicKey,
    curator: curatorKp.publicKey,
    debtBank: new PublicKey(market.debtBank),
    market: new PublicKey(market.market),
    subVaultId: input.subVaultId,
  });
  const sig = await sendIxs(conn, feePayer, [ix], [curatorKp]);
  log(`[vault-cancel-order] signature = ${sig}`);
  appendTxLog({
    script: 'vault-cancel-order',
    signatures: [sig],
    summary: {
      marketLabel: input.marketLabel,
      mint: input.mint,
      subVaultId: input.subVaultId,
      curator: curatorKp.publicKey.toBase58(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
