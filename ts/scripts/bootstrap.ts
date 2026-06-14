/**
 * bootstrap.ts — one-shot post-deploy chain.
 *
 * Runs the action scripts in order, each shelling out via `tsx` so they
 * stay isolated (independent processes, independent connections, fresh
 * dotenv). Every step is idempotent — the underlying scripts skip work
 * when the resulting `.local/*.json` artifact already covers it.
 *
 * Steps:
 *   1. create-global-config.ts
 *   2. create-curators.ts            (1 curator by default)
 *   3. create-vault.ts               (USDC vault)
 *   4. setup-curator-sub-vaults.ts     (3 subVaults in the USDC vault)
 *   5. init-market.ts                (USDC/SOL + USDC/JitoSOL)
 *
 * `deploy.ts` is intentionally separate — bootstrap assumes
 * `.local/protocol.json` already exists.
 *
 * Required `.local/*` input files (operator must seed these or accept
 * the defaults provided by `.local.example`):
 *   curators-input.json          (required)
 *   vault-input.json             (required: { mint: <USDC> })
 *   setup-curator-sub-vaults-input.json (required: { mint, subVaults: [...] })
 *   markets-input.json           (required: { markets: [...] })
 */
import { spawnSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as path from 'node:path';

import {
  ensureLocalDir,
  localPath,
  log,
  readJsonOptional,
  REPO_ROOT,
} from './_runner.js';

interface BootstrapInputCheck {
  filename: string;
  required: boolean;
  description: string;
}

const INPUT_FILES: BootstrapInputCheck[] = [
  { filename: 'protocol.json', required: true, description: 'output of `yarn deploy`' },
  {
    filename: 'mainnet-banks.json',
    required: true,
    description: 'static P0 bank registry (copy from .local.example/mainnet-banks.json)',
  },
  { filename: 'vault-input.json', required: true, description: 'USDC vault input' },
  {
    filename: 'setup-curator-sub-vaults-input.json',
    required: true,
    description: 'curator + 3 sub-vaults config',
  },
  { filename: 'markets-input.json', required: true, description: 'market init input' },
];

function checkInputs(): void {
  let bad = false;
  for (const f of INPUT_FILES) {
    const exists = fs.existsSync(localPath(f.filename));
    if (!exists && f.required) {
      log(`[bootstrap] MISSING .local/${f.filename}  (${f.description})`);
      bad = true;
    }
  }
  if (bad) {
    throw new Error(
      `bootstrap: missing required .local/ inputs. Copy from .local.example/ ` +
      `and fill in mints/banks, then re-run.`,
    );
  }
}

function runScript(name: string): void {
  const script = path.join(REPO_ROOT, 'ts', 'scripts', name);
  if (!fs.existsSync(script)) {
    throw new Error(`bootstrap step missing: ${script}`);
  }
  log(`\n[bootstrap] === ${name} ===`);
  const r = spawnSync('yarn', ['tsx', script], {
    stdio: 'inherit',
    cwd: REPO_ROOT,
  });
  if (r.status !== 0) {
    throw new Error(`bootstrap step ${name} exited with status ${r.status ?? 'null'}`);
  }
}

interface BootstrapStep {
  script: string;
  /**
   * Returns true only when the step is FULLY complete, so the orchestrator
   * can skip it without spawning. Omit for steps whose own per-item skip
   * (e.g. per-subVault-row) should always run — passing a partial output here
   * would wrongly skip the remaining items.
   */
  complete?: () => boolean;
}

const STEPS: BootstrapStep[] = [
  {
    script: 'create-global-config.ts',
    complete: () => readJsonOptional('global-config.json') !== null,
  },
  {
    script: 'create-curators.ts',
    complete: () => readJsonOptional('curators.json') !== null,
  },
  {
    // Done when the vault for the requested mint is already recorded.
    script: 'create-vault.ts',
    complete: () => {
      const input = readJsonOptional<{ mint: string }>('vault-input.json');
      const vaults = readJsonOptional<Record<string, unknown>>('vaults.json');
      return !!input && !!vaults && vaults[input.mint] !== undefined;
    },
  },
  // Per-subVault-row idempotent — always delegate so partial setups finish.
  { script: 'setup-curator-sub-vaults.ts' },
  {
    // Done only when every requested market label is already recorded;
    // a partial set falls through so the remaining markets get created.
    script: 'init-market.ts',
    complete: () => {
      const input = readJsonOptional<{ markets: { label: string }[] }>('markets-input.json');
      const markets = readJsonOptional<Record<string, unknown>>('markets.json');
      if (!input || !markets) return false;
      return input.markets.every((m) => markets[m.label] !== undefined);
    },
  },
];

function main(): void {
  ensureLocalDir();
  checkInputs();
  for (const step of STEPS) {
    if (step.complete?.()) {
      log(`\n[bootstrap] === ${step.script} === already complete; skipping`);
      continue;
    }
    runScript(step.script);
  }

  // Sanity: surface the final artifact summary so the operator can eyeball it.
  const protocol = readJsonOptional<unknown>('protocol.json');
  const config = readJsonOptional<unknown>('global-config.json');
  const vaults = readJsonOptional<Record<string, unknown>>('vaults.json') ?? {};
  const subVaults = readJsonOptional<Record<string, unknown[]>>('sub-vaults.json') ?? {};
  const markets = readJsonOptional<Record<string, unknown>>('markets.json') ?? {};

  log('\n[bootstrap] === summary ===');
  log(`protocol: ${protocol ? 'OK' : 'MISSING'}`);
  log(`global-config: ${config ? 'OK' : 'MISSING'}`);
  log(`vaults: ${Object.keys(vaults).length}`);
  log(
    `subVaults: ${Object.values(subVaults).reduce((acc, arr) => acc + (arr?.length ?? 0), 0)}`,
  );
  log(`markets: ${Object.keys(markets).length}`);
  log(`\n[bootstrap] done.`);
}

main();
