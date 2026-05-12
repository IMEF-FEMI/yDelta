#!/usr/bin/env -S npx ts-node
/* eslint-disable no-console */
import * as fs from 'fs';
import * as path from 'path';
import { Connection, Keypair, PublicKey } from '@solana/web3.js';
import {
  assertNotDevnet,
  confirmMainnetIfNeeded,
  defaultCluster,
  defaultPayerPath,
  fmtKey,
  header,
  LOCAL_DIR,
  loadDotEnv,
  loadKeypair,
  makeConnection,
  optionalArg,
  parseArgs,
  sendIxs,
} from './common';
import { Market } from '../../client/ts/src/market';
import { createSetMarketPauseInstruction } from '../../client/ts/src/ydelta/instructions';

/* USAGE:
 *
 *   yarn setup:unpause-market                            # unpause every market in .local/markets/
 *   yarn setup:unpause-market --market <PUBKEY>          # unpause one market
 *   yarn setup:unpause-market --dry-run                  # show what would happen, no tx
 *
 * Signer must be the market admin (= GlobalConfig.protocol_admin at create
 * time). Idempotent — already-live markets are skipped without sending a tx.
 */

type Target = { label: string; market: PublicKey };

function discoverFromLocalDir(): Target[] {
  const dir = path.join(LOCAL_DIR, 'markets');
  if (!fs.existsSync(dir)) return [];
  const out: Target[] = [];
  for (const f of fs.readdirSync(dir)) {
    if (!f.endsWith('.json') || f.endsWith('.keypair.json')) continue;
    try {
      const j = JSON.parse(fs.readFileSync(path.join(dir, f), 'utf8'));
      if (typeof j.market !== 'string') continue;
      out.push({
        label: typeof j.name === 'string' && j.name.length > 0 ? j.name : f.replace(/\.json$/, ''),
        market: new PublicKey(j.market),
      });
    } catch {
      // skip malformed
    }
  }
  return out;
}

async function unpauseOne(
  connection: Connection,
  payer: Keypair,
  t: Target,
  dryRun: boolean,
): Promise<'unpaused' | 'already-live' | 'wrong-admin'> {
  const market = await Market.loadFromAddress({ connection, address: t.market });
  if (!market.isPaused()) {
    console.log(`  · ${t.label}  ${fmtKey(t.market)}  already LIVE — skip`);
    return 'already-live';
  }
  if (!market.admin().equals(payer.publicKey)) {
    console.log(
      `  ✗ ${t.label}  ${fmtKey(t.market)}  signer ${fmtKey(payer.publicKey)} != market.admin ${fmtKey(market.admin())} — skip`,
    );
    return 'wrong-admin';
  }
  if (dryRun) {
    console.log(`  → ${t.label}  ${fmtKey(t.market)}  would unpause (dry-run)`);
    return 'unpaused';
  }
  const ix = createSetMarketPauseInstruction(
    { admin: payer.publicKey, market: t.market },
    { paused: false },
  );
  const sig = await sendIxs(connection, payer, [ix], [], { finalize: true });
  console.log(`  ✓ ${t.label}  ${fmtKey(t.market)}  unpaused  tx=${sig}`);
  return 'unpaused';
}

async function main(): Promise<void> {
  loadDotEnv();
  const args = parseArgs(process.argv.slice(2));
  const cluster = optionalArg(args, 'cluster') ?? defaultCluster();
  if (!cluster) throw new Error('missing required --cluster (or YDELTA_CLUSTER in .env)');
  const payer = loadKeypair(optionalArg(args, 'payer') ?? defaultPayerPath());
  const connection = makeConnection(cluster);
  assertNotDevnet(cluster, connection.rpcEndpoint);
  await confirmMainnetIfNeeded(connection, cluster, args);

  const dryRun = args['dry-run'] === true;
  const single = optionalArg(args, 'market');
  const targets: Target[] = single
    ? [{ label: single, market: new PublicKey(single) }]
    : discoverFromLocalDir();
  if (targets.length === 0) {
    console.log(`No markets to unpause. Pass --market <pk> or populate .local/markets/.`);
    return;
  }

  header(`unpause-market → ${cluster}${dryRun ? ' (DRY RUN)' : ''}`);
  console.log(`  RPC:    ${connection.rpcEndpoint}`);
  console.log(`  Signer: ${fmtKey(payer.publicKey)}`);
  console.log(`  Markets: ${targets.length}`);

  let unpaused = 0;
  let alreadyLive = 0;
  let wrongAdmin = 0;
  for (const t of targets) {
    try {
      const r = await unpauseOne(connection, payer, t, dryRun);
      if (r === 'unpaused') unpaused++;
      else if (r === 'already-live') alreadyLive++;
      else wrongAdmin++;
    } catch (err) {
      console.error(`  ✗ ${t.label} ${fmtKey(t.market)}: ${(err as Error).message}`);
      throw err;
    }
  }

  console.log(`\n  ${unpaused} unpaused, ${alreadyLive} already live, ${wrongAdmin} wrong-admin`);
  if (wrongAdmin > 0) process.exitCode = 2;
}

main().catch((err) => {
  console.error('\nUNPAUSE-MARKET FAILED:');
  console.error((err as Error).stack ?? (err as Error).message);
  process.exit(1);
});
