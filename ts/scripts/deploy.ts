/**
 * deploy.ts — wrapper around `scripts/deploy-program.sh` plus an
 * idempotency layer.
 *
 * Detect-and-skip cascade:
 *
 *   1. `.local/protocol.json` exists  → already done; no-op.
 *   2. Otherwise                      → invoke the shell deploy.
 *
 * The shell is the single source of truth for both fresh deploys and
 * "already on-chain, recover protocol.json" flows. Keep the TS wrapper
 * thin so we don't fork authority/program-data discovery logic and write
 * incorrect recovery artifacts.
 *
 * Optional shell args: `YDELTA_DEPLOY_ARGS="--skip-build --yes"`.
 *
 * Writes: .local/protocol.json
 */
import { spawnSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as path from 'node:path';

import {
  appendTxLog,
  ensureLocalDir,
  localPath,
  log,
  REPO_ROOT,
} from './_runner.js';

async function main(): Promise<void> {
  ensureLocalDir();

  if (fs.existsSync(localPath('protocol.json'))) {
    log(`[deploy] .local/protocol.json already exists — nothing to do (use scripts/upgrade-program.sh to redeploy)`);
    return;
  }

  const shell = path.join(REPO_ROOT, 'scripts', 'deploy-program.sh');
  if (!fs.existsSync(shell)) {
    throw new Error(`missing ${shell} — did you delete the deploy shell script?`);
  }
  const extra = (process.env.YDELTA_DEPLOY_ARGS ?? '').trim();
  const args = extra.length > 0 ? extra.split(/\s+/) : [];
  log(`[deploy] running ${shell} ${args.join(' ')}`);
  const r = spawnSync('bash', [shell, ...args], { stdio: 'inherit' });
  if (r.status !== 0) {
    throw new Error(`deploy-program.sh exited with status ${r.status ?? 'null'}`);
  }
  const protocol = localPath('protocol.json');
  if (!fs.existsSync(protocol)) {
    throw new Error(`deploy succeeded but ${protocol} is missing`);
  }
  log(`[deploy] wrote ${protocol}`);
  appendTxLog({
    script: 'deploy',
    signatures: [],
    summary: { protocolJson: protocol, shellManagedRecovery: true },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
