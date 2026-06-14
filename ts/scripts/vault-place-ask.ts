/**
 * vault-place-ask.ts — `PlaceOrderForSubVault` (tag 12). Curator publishes
 * (or repositions) a vault ask on a market.
 *
 * v1: takes **no** rate or term — the processor computes
 * `live bank lending APR + subVault.spreadBps` and uses
 * `subVault.maxTermSeconds`. Only `flags` carries the order bitmask.
 *
 * Reads:
 *   .local/vault-place-ask-input.json {
 *     marketLabel: string,                   // .local/markets.json key
 *     mint: string,                          // debt mint (must equal market.debtMint)
 *     subVaultId: number,
 *     flags?: number
 *   }
 *   .local/markets.json
 *   .local/sub-vaults.json
 *   .local/curators.json
 *
 * The curator key is loaded from `curators.json` via the sub-vault's
 * `curatorLabel`. Fee payer = deployer (signer); curator-signs only.
 */
import { PublicKey } from '@solana/web3.js';

import { placeOrderForSubVaultInstruction } from '../src/instructions/index.js';
import {
  appendTxLog,
  loadConnection,
  loadSigner,
  log,
  readJson,
  readKeypairFromBase58,
  sendIxs,
} from './_runner.js';
import type { MarketDump, SubVaultDump, CuratorDump } from './_types.js';
import { resolveCuratorForSubVault } from './_types.js';

interface Input {
  marketLabel: string;
  mint: string;
  subVaultId: number;
  flags?: number;
}

async function main(): Promise<void> {
  const input = readJson<Input>('vault-place-ask-input.json');
  const markets = readJson<Record<string, MarketDump>>('markets.json');
  const subVaults = readJson<Record<string, SubVaultDump[]>>('sub-vaults.json');
  const curators = readJson<CuratorDump[]>('curators.json');

  const market = markets[input.marketLabel];
  if (!market) throw new Error(`unknown marketLabel ${input.marketLabel}`);
  if (market.debtMint !== input.mint) {
    throw new Error(
      `market ${input.marketLabel} debtMint ${market.debtMint} ≠ input.mint ${input.mint}`,
    );
  }
  const curator = resolveCuratorForSubVault(subVaults, curators, input.mint, input.subVaultId);
  const curatorKp = readKeypairFromBase58(curator.secretKeyBase58);

  const conn = loadConnection();
  const feePayer = loadSigner();

  const ix = placeOrderForSubVaultInstruction({
    feePayer: feePayer.publicKey,
    curator: curatorKp.publicKey,
    market: new PublicKey(market.market),
    debtBank: new PublicKey(market.debtBank),
    marginfiGroup: new PublicKey(market.marginfiGroup),
    collateralBank: new PublicKey(market.collateralBank),
    debtOracles: market.debtOracles.map((o) => new PublicKey(o)),
    collateralOracles: market.collateralOracles.map((o) => new PublicKey(o)),
    subVaultId: input.subVaultId,
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
      subVaultId: input.subVaultId,
      curator: curatorKp.publicKey.toBase58(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
