/**
 * convert-idl-to-anchor.ts — convert the Shank-style ts/idl/ydelta.json
 * to a modern Anchor-v0.30+ IDL with explicit per-instruction
 * `discriminator` arrays (so Solana Explorer / Solscan decode our 1-byte
 * tag-prefixed instructions).
 *
 * Output: ts/idl/ydelta-anchor.json
 *
 * Run: yarn tsx ts/scripts/convert-idl-to-anchor.ts
 *
 * The Shank shape has:
 *   { instructions: [{ name, tag, accounts: [{ name, signer, writable, pda }], args }] }
 *
 * The Anchor 0.30+ shape needs:
 *   { address, metadata, instructions: [{ name, discriminator: [tag],
 *     accounts: [{ name, writable, signer }], args }] }
 *
 * Notes:
 *  - Field renames: signer→signer, writable→writable (same names, just
 *    different defaults — Anchor omits `false`, we'll keep things explicit).
 *  - Names go from snake_case → camelCase for Anchor convention. Explorer
 *    displays whatever we emit.
 *  - Error codes stay as-is (the on-chain custom-error code is the raw
 *    discriminant, not Anchor's 6000+ offset).
 *  - PDA hints aren't carried over — Anchor expects a structured `pda`
 *    object that's harder to express for our seed strings; explorers
 *    don't need them for decoding.
 */
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, '../..');

const inPath = resolve(REPO_ROOT, 'ts/idl/ydelta.json');
const outPath = resolve(REPO_ROOT, 'ts/idl/ydelta-anchor.json');

interface ShankIx {
  name: string;
  tag: number;
  note?: string;
  splitPayer?: boolean;
  accounts: Array<{
    name: string;
    signer?: boolean;
    writable?: boolean;
    pda?: unknown;
    note?: string;
  }>;
  args: Array<{ name: string; type: unknown }>;
}

interface ShankAccount {
  name: string;
  size?: number;
  encoding?: string;
  discriminator?: string;
  note?: string;
}

interface ShankError {
  code: number;
  name: string;
  msg?: string;
}

interface ShankIdl {
  version: string;
  name: string;
  programId: string;
  description?: string;
  instructions: ShankIx[];
  accounts?: ShankAccount[];
  errors?: ShankError[];
}

function snakeToCamel(s: string): string {
  return s.replace(/_([a-z0-9])/g, (_, c) => c.toUpperCase());
}

function pascalToCamel(s: string): string {
  return s.length === 0 ? s : s[0].toLowerCase() + s.slice(1);
}

/** Recursively remap Shank type vocabulary to Anchor's. Anchor's parser
 *  rejects `"publicKey"` — its lowercase `"pubkey"` is the canonical name. */
function remapType(t: unknown): unknown {
  if (typeof t === 'string') {
    if (t === 'publicKey') return 'pubkey';
    return t;
  }
  if (Array.isArray(t)) return t.map(remapType);
  if (t && typeof t === 'object') {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(t as Record<string, unknown>)) {
      out[k] = remapType(v);
    }
    return out;
  }
  return t;
}

const shank: ShankIdl = JSON.parse(readFileSync(inPath, 'utf-8'));

const anchorIdl = {
  address: shank.programId,
  metadata: {
    name: shank.name,
    version: shank.version,
    spec: '0.1.0',
    description: shank.description ?? '',
  },
  instructions: shank.instructions.map((ix) => ({
    name: pascalToCamel(ix.name),
    // 1-byte tag prefix is our instruction discriminator. Explorer
    // matches the first N bytes of the tx instruction data; our
    // processor reads byte 0 then borsh-deserialises the rest.
    discriminator: [ix.tag],
    accounts: ix.accounts.map((a) => ({
      name: snakeToCamel(a.name),
      writable: !!a.writable,
      signer: !!a.signer,
    })),
    args: ix.args.map((a) => ({
      name: snakeToCamel(a.name),
      type: remapType(a.type),
    })),
  })),
  accounts: (shank.accounts ?? []).map((a) => ({
    name: a.name,
    // Anchor expects a discriminator on accounts. Our Pod accounts have
    // 8-byte discriminators per the `MARKET_FIXED_DISCRIMINANT` consts;
    // for explorer purposes we leave this as zero bytes — the decoder
    // identifies accounts by owner + structural fields, not by
    // discriminator alone. Refining this would need to expose the
    // 8-byte LE-encoded discriminant per type.
    discriminator: [0, 0, 0, 0, 0, 0, 0, 0],
  })),
  errors: (shank.errors ?? []).map((e) => ({
    code: e.code,
    name: e.name,
    msg: e.msg ?? e.name,
  })),
  // Anchor's `types` section is for custom non-account structs. Our IDL
  // args use only primitives + Option<primitive>, so this is empty.
  types: [] as unknown[],
};

writeFileSync(outPath, JSON.stringify(anchorIdl, null, 2) + '\n');

const sizeKb = (JSON.stringify(anchorIdl).length / 1024).toFixed(1);
console.log(`Wrote ${outPath} (${sizeKb} KB)`);
console.log(`  ${anchorIdl.instructions.length} instructions`);
console.log(`  ${anchorIdl.accounts.length} accounts`);
console.log(`  ${anchorIdl.errors.length} errors`);
