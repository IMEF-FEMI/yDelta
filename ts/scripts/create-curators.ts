/**
 * create-curators.ts — generate curator keypairs and persist them as
 * base58 secret keys in `.local/curators.json`.
 *
 * Reads (optional):
 *   .local/curators-input.json {
 *     curators?: [{ label: string }]
 *   }
 *
 * Writes:
 *   .local/curators.json [
 *     { label, pubkey, secretKeyBase58 }
 *   ]
 *
 * Idempotent by default: if `curators.json` already exists we keep it.
 * Set `YDELTA_REGEN_CURATORS=1` to overwrite with a fresh set.
 */
import { Keypair } from '@solana/web3.js';
import bs58 from 'bs58';

import {
  ensureLocalDir,
  localPath,
  log,
  readJsonOptional,
  writeJson,
} from './_runner.js';
import type { CuratorDump } from './_types.js';

interface Input {
  curators?: Array<{
    label: string;
  }>;
}

const DEFAULT_LABELS = ['curator-main'];

function desiredLabels(input: Input | null): string[] {
  const labels = input?.curators?.map((c) => c.label.trim()).filter((c) => c.length > 0) ?? [];
  return labels.length > 0 ? labels : DEFAULT_LABELS;
}

function makeCurator(label: string): CuratorDump {
  const kp = Keypair.generate();
  return {
    label,
    pubkey: kp.publicKey.toBase58(),
    secretKeyBase58: bs58.encode(kp.secretKey),
  };
}

function main(): void {
  ensureLocalDir();
  const input = readJsonOptional<Input>('curators-input.json');
  const labels = desiredLabels(input);
  const regen = process.env.YDELTA_REGEN_CURATORS === '1';
  const outputPath = localPath('curators.json');

  if (!regen) {
    const existing = readJsonOptional<CuratorDump[]>('curators.json');
    if (existing) {
      log(`[create-curators] ${outputPath} already exists; keeping existing curators`);
      return;
    }
  }

  const unique = new Set<string>();
  const curators: CuratorDump[] = [];
  for (const label of labels) {
    if (unique.has(label)) {
      throw new Error(`duplicate curator label in curators-input.json: ${label}`);
    }
    unique.add(label);
    curators.push(makeCurator(label));
  }

  writeJson('curators.json', curators);
  log(`[create-curators] wrote ${curators.length} curator keypairs to ${outputPath}`);
}

main();
