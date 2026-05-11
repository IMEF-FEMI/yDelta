#!/usr/bin/env -S npx ts-node
/**
 * encode-keypairs.ts
 *
 * Encode Solana keypair JSON files (the Solana-CLI byte-array format
 * produced by `solana-keygen` and `yarn setup:risk-profiles`) into
 * base58 strings ready to paste into env-var fields for the cranker
 * (or any PaaS that only supports env-var secrets, e.g. Railway).
 *
 * Outputs `KEY=value` lines to stdout — one per role — so you can scan
 * them, copy, and paste into the deploy provider's UI. The values
 * never touch disk; clear scrollback afterwards (`clear; history -c`).
 *
 * Usage (run from ydelta/ root):
 *
 *   # Fee-payer only:
 *   yarn setup:encode-keypairs -- --fee-payer ~/.config/solana/id.json
 *
 *   # Curators from the risk-profile output map (single JSON file
 *   # containing all 5 curators keyed by profile_id):
 *   yarn setup:encode-keypairs -- \
 *     --curator-map .local/curators/mainnet-beta/<DEBT_MINT>.json
 *
 *   # Both at once:
 *   yarn setup:encode-keypairs -- \
 *     --fee-payer ~/.config/solana/id.json \
 *     --curator-map .local/curators/mainnet-beta/<DEBT_MINT>.json
 *
 *   # Single keypair JSON file (e.g. already split out of the map):
 *   yarn setup:encode-keypairs -- \
 *     --keypair ./curator-3.json --as CURATOR_BALANCED_BASE58
 *
 * Zero external deps — uses only Node built-ins. Base58 is implemented
 * inline to keep the dep graph thin.
 */

import { readFileSync } from 'fs';
import { homedir } from 'os';
import * as path from 'path';

// ─── Base58 (Bitcoin alphabet — same as Solana) ─────────────────────────
const B58_ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

function b58encode(bytes: Uint8Array): string {
  let n = 0n;
  for (const b of bytes) n = (n << 8n) | BigInt(b);
  let out = '';
  while (n > 0n) {
    const r = Number(n % 58n);
    n = n / 58n;
    out = B58_ALPHABET[r] + out;
  }
  // Each leading zero byte becomes a '1' in base58.
  for (const b of bytes) {
    if (b === 0) out = '1' + out;
    else break;
  }
  return out;
}

// ─── Args ────────────────────────────────────────────────────────────────
type Args = {
  feePayer?: string;
  curatorMap?: string;
  singleKeypair?: string;
  singleAs?: string;
};

function parseArgs(argv: string[]): Args {
  const args: Args = {};
  for (let i = 0; i < argv.length; i++) {
    const k = argv[i];
    const v = argv[i + 1];
    switch (k) {
      case '--fee-payer':
        args.feePayer = v;
        i++;
        break;
      case '--curator-map':
        args.curatorMap = v;
        i++;
        break;
      case '--keypair':
        args.singleKeypair = v;
        i++;
        break;
      case '--as':
        args.singleAs = v;
        i++;
        break;
      case '-h':
      case '--help':
        console.error(banner());
        process.exit(0);
      default:
        console.error(`unknown arg: ${k}\n`);
        console.error(banner());
        process.exit(1);
    }
  }
  return args;
}

function banner(): string {
  return `Usage:
  yarn setup:encode-keypairs -- [options]

Options:
  --fee-payer <path>     Path to a Solana CLI keypair JSON (the
                         64-element u8 array). Emits FEE_PAYER_KEYPAIR_BASE58
                         and FEE_PAYER_PUBKEY.
  --curator-map <path>   Path to the curators JSON written by
                         \`yarn setup:risk-profiles\` (a map keyed by
                         profile_id). Emits one CURATOR_<NAME>_BASE58
                         per entry plus a pubkey comment.
  --keypair <path>       Single keypair JSON file.
  --as <ENV_NAME>        Env-var name to use with --keypair.
  -h, --help             Print this and exit.

Outputs KEY=value lines to stdout; you copy-paste them into your deploy
provider's Variables tab. No persistence — clear scrollback afterwards.`;
}

// ─── Helpers ─────────────────────────────────────────────────────────────
function expandHome(p: string): string {
  return p.startsWith('~') ? path.join(homedir(), p.slice(1)) : path.resolve(p);
}

function loadKeypairBytes(p: string): Uint8Array {
  const raw = readFileSync(expandHome(p), 'utf8');
  const arr = JSON.parse(raw);
  if (!Array.isArray(arr) || arr.length !== 64) {
    throw new Error(
      `${p}: expected a 64-element JSON byte array, got ${
        typeof arr === 'object' ? `length ${arr?.length ?? '?'}` : typeof arr
      }`,
    );
  }
  return Uint8Array.from(arr);
}

// Derive the public key from the secret (last 32 bytes of the 64-byte
// ed25519 secret-key = public key per Solana's convention).
function pubkeyFromSecret(secret: Uint8Array): string {
  return b58encode(secret.slice(32, 64));
}

function emitFeePayer(p: string): void {
  const bytes = loadKeypairBytes(p);
  console.log(`# fee-payer pubkey: ${pubkeyFromSecret(bytes)}`);
  console.log(`FEE_PAYER_PUBKEY=${pubkeyFromSecret(bytes)}`);
  console.log(`FEE_PAYER_KEYPAIR_BASE58=${b58encode(bytes)}`);
  console.log('');
}

function emitCuratorMap(p: string): void {
  const raw = readFileSync(expandHome(p), 'utf8');
  const data: Record<string, { pubkey: string; profileName: string; secretKey: number[] }> =
    JSON.parse(raw);
  const ids = Object.keys(data).sort((a, b) => Number(a) - Number(b));
  console.log(`# ${ids.length} curators from ${p}`);
  for (const id of ids) {
    const entry = data[id];
    const bytes = Uint8Array.from(entry.secretKey);
    const envName = `CURATOR_${entry.profileName.toUpperCase()}_BASE58`;
    console.log(`# profile_id=${id} (${entry.profileName}) pubkey=${entry.pubkey}`);
    console.log(`${envName}=${b58encode(bytes)}`);
    console.log('');
  }
}

function emitSingle(p: string, envName: string): void {
  const bytes = loadKeypairBytes(p);
  console.log(`# pubkey: ${pubkeyFromSecret(bytes)}`);
  console.log(`${envName}=${b58encode(bytes)}`);
  console.log('');
}

// ─── Main ────────────────────────────────────────────────────────────────
function main(): void {
  const args = parseArgs(process.argv.slice(2));

  if (!args.feePayer && !args.curatorMap && !args.singleKeypair) {
    console.error('error: provide at least one of --fee-payer / --curator-map / --keypair\n');
    console.error(banner());
    process.exit(1);
  }
  if (args.singleKeypair && !args.singleAs) {
    console.error('error: --keypair requires --as <ENV_NAME>');
    process.exit(1);
  }

  if (args.feePayer) emitFeePayer(args.feePayer);
  if (args.curatorMap) emitCuratorMap(args.curatorMap);
  if (args.singleKeypair && args.singleAs) emitSingle(args.singleKeypair, args.singleAs);
}

try {
  main();
} catch (err) {
  console.error(`encode-keypairs failed: ${(err as Error).message}`);
  process.exit(1);
}
