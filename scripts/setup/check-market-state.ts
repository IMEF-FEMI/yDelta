#!/usr/bin/env -S npx ts-node
/* eslint-disable no-console */
import * as fs from 'fs';
import * as path from 'path';
import { Connection, PublicKey } from '@solana/web3.js';
import {
  assertNotDevnet,
  defaultCluster,
  fmtKey,
  header,
  LOCAL_DIR,
  loadDotEnv,
  makeConnection,
  optionalArg,
  parseArgs,
} from './common';
import { Market } from '../../client/ts/src/market';
import { GlobalConfig } from '../../client/ts/src/globalConfig';

/* USAGE:
 *
 *   yarn setup:check-market-state                            # all markets in .local/markets/
 *   yarn setup:check-market-state --market <PUBKEY>          # single market
 *
 * Reports pause state (global + per-market) plus the market admin.
 * Read-only — no signer needed. Exits 2 if any market is paused
 * (override with --allow-paused).
 */

type Row = {
  label: string;
  market: PublicKey;
  paused?: boolean;
  admin?: PublicKey;
  error?: string;
};

function discoverFromLocalDir(): { label: string; market: PublicKey }[] {
  const dir = path.join(LOCAL_DIR, 'markets');
  if (!fs.existsSync(dir)) return [];
  const out: { label: string; market: PublicKey }[] = [];
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

async function main(): Promise<void> {
  loadDotEnv();
  const args = parseArgs(process.argv.slice(2));
  const cluster = optionalArg(args, 'cluster') ?? defaultCluster();
  if (!cluster) throw new Error('missing required --cluster (or YDELTA_CLUSTER in .env)');
  const connection = makeConnection(cluster);
  assertNotDevnet(cluster, connection.rpcEndpoint);

  header(`check-market-state → ${cluster}`);
  console.log(`  RPC: ${connection.rpcEndpoint}`);

  // ─── GlobalConfig pause state ─────────────────────────────────────────
  const gc = await GlobalConfig.load({ connection });
  if (!gc) {
    console.log(`  GlobalConfig: <not found> — has deploy.ts been run on this cluster?`);
  } else {
    console.log(`  GlobalConfig.is_paused: ${gc.isPaused()}`);
    console.log(`  GlobalConfig.protocol_admin: ${fmtKey(gc.protocolAdmin())}`);
  }

  // ─── Resolve which markets to check ──────────────────────────────────
  const single = optionalArg(args, 'market');
  const targets = single
    ? [{ label: single, market: new PublicKey(single) }]
    : discoverFromLocalDir();
  if (targets.length === 0) {
    console.log(`\n  No markets to check. Pass --market <pk> or populate .local/markets/.`);
    return;
  }

  header(`Checking ${targets.length} market(s)`);

  const rows: Row[] = [];
  for (const t of targets) {
    try {
      const market = await Market.loadFromAddress({ connection, address: t.market });
      rows.push({
        label: t.label,
        market: t.market,
        paused: market.isPaused(),
        admin: market.admin(),
      });
    } catch (err) {
      rows.push({ label: t.label, market: t.market, error: (err as Error).message });
    }
  }

  // ─── Print table ─────────────────────────────────────────────────────
  for (const r of rows) {
    const tag = r.label.length > 24 ? r.label.slice(0, 21) + '...' : r.label.padEnd(24);
    if (r.error) {
      console.log(`  ${tag}  ${fmtKey(r.market)}  ERROR: ${r.error}`);
      continue;
    }
    const stateTag = r.paused ? 'PAUSED ' : 'LIVE   ';
    console.log(`  ${tag}  ${fmtKey(r.market)}  ${stateTag}  admin=${fmtKey(r.admin!)}`);
  }

  const pausedCount = rows.filter((r) => r.paused === true).length;
  const liveCount = rows.filter((r) => r.paused === false).length;
  const errorCount = rows.filter((r) => r.error !== undefined).length;
  console.log(`\n  ${liveCount} live, ${pausedCount} paused, ${errorCount} error`);

  // Non-zero exit if any market is paused — handy for scripted checks.
  // Override with --allow-paused to always exit 0.
  if (pausedCount > 0 && args['allow-paused'] !== true) {
    process.exitCode = 2;
  }
}

main().catch((err) => {
  console.error('\nCHECK-MARKET-STATE FAILED:');
  console.error((err as Error).stack ?? (err as Error).message);
  process.exit(1);
});
