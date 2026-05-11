/* eslint-disable no-console */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  Commitment,
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
  sendAndConfirmTransaction,
} from '@solana/web3.js';

/** Map shorthand cluster names to RPC endpoints. yDelta targets localhost
 *  (dev/test) and mainnet-beta (production) only — devnet/testnet are
 *  intentionally absent and rejected by `assertNotDevnet`. */
export const CLUSTER_RPC: Record<string, string> = {
  localhost: 'http://127.0.0.1:8899',
  localnet: 'http://127.0.0.1:8899',
  'mainnet-beta': 'https://api.mainnet-beta.solana.com',
  mainnet: 'https://api.mainnet-beta.solana.com',
};

/** Reject any cluster identifier or RPC URL that points at Solana
 *  devnet/testnet. yDelta is deployed only to localhost or mainnet-beta.
 *  Catching this before any signing happens prevents a misconfigured
 *  `--cluster` flag or `YDELTA_CLUSTER` env from sending live txs to a
 *  cluster the protocol will never support. Call from every setup script
 *  right after `makeConnection`. */
export function assertNotDevnet(cluster: string, rpcUrl: string): void {
  const c = cluster.toLowerCase();
  const r = rpcUrl.toLowerCase();
  if (c === 'devnet' || c === 'testnet' || r.includes('devnet.solana.com') || r.includes('testnet.solana.com')) {
    throw new Error(
      `refusing to run: cluster "${cluster}" resolves to ${rpcUrl}.\n` +
        `yDelta is only deployed to localhost or mainnet-beta — devnet/testnet are not supported targets.`,
    );
  }
}

/** Parse a `.env` file at the repo root and merge into process.env without
 *  overwriting existing variables (process.env wins). No external dep — we
 *  support `KEY=value`, `KEY="value with spaces"`, `# comments`, and blank
 *  lines. Call once at the start of each setup script's `main()`. */
export function loadDotEnv(filePath?: string): void {
  const target = filePath ?? path.join(REPO_ROOT, '.env');
  if (!fs.existsSync(target)) return;
  const raw = fs.readFileSync(target, 'utf8');
  for (const line of raw.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) continue;
    const eq = trimmed.indexOf('=');
    if (eq <= 0) continue;
    const key = trimmed.slice(0, eq).trim();
    let value = trimmed.slice(eq + 1).trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    if (process.env[key] === undefined) process.env[key] = value;
  }
}

// Full 44-char base58 mainnet-beta genesis hash. A truncated hash here would
// silently bypass the mainnet warning for custom RPC endpoints whose URL does
// not include the literal "mainnet-beta".
const MAINNET_BETA_GENESIS_HASH = '5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpvUUVfNFhgzbm';
if (MAINNET_BETA_GENESIS_HASH.length !== 44) {
  throw new Error('MAINNET_BETA_GENESIS_HASH must be a 44-char base58 hash');
}

export function resolveRpc(cluster: string): string {
  if (cluster.startsWith('http://') || cluster.startsWith('https://')) return cluster;
  // `YDELTA_RPC_URL` overrides the cluster→URL map (e.g. for a custom
  // mainnet RPC provider). Honored regardless of cluster shorthand.
  const envUrl = process.env.YDELTA_RPC_URL;
  if (envUrl) return envUrl;
  const url = CLUSTER_RPC[cluster];
  if (!url) throw new Error(`unknown cluster '${cluster}'; pass a full URL or one of ${Object.keys(CLUSTER_RPC).join(', ')}`);
  return url;
}

/** If the target cluster resolves to mainnet-beta, require the operator to
 *  type the literal phrase `mainnet-beta` on stdin before proceeding. Pass
 *  `--confirm-mainnet` (or `YDELTA_CONFIRM_MAINNET=1`) to skip the prompt for
 *  CI/automation. */
export async function confirmMainnetIfNeeded(
  connection: Connection,
  cluster: string,
  args: Record<string, string | string[] | boolean>,
): Promise<void> {
  if (args['confirm-mainnet'] === true) return;
  if (process.env.YDELTA_CONFIRM_MAINNET === '1') return;

  const clusterHint = cluster.toLowerCase();
  let isMainnet =
    clusterHint === 'mainnet' ||
    clusterHint === 'mainnet-beta' ||
    connection.rpcEndpoint.includes('mainnet-beta');

  if (!isMainnet) {
    try {
      isMainnet = (await connection.getGenesisHash()) === MAINNET_BETA_GENESIS_HASH;
    } catch {
      return;
    }
  }

  if (!isMainnet) return;

  console.warn(`\n!!! MAINNET TARGET: ${connection.rpcEndpoint}`);
  console.warn(`!!! Type "mainnet-beta" to proceed (CTRL-C to abort).`);
  const answer = (await readLine('mainnet-beta> ')).trim();
  if (answer !== 'mainnet-beta') {
    throw new Error(`mainnet confirmation declined (got "${answer}")`);
  }
}

/** Prompt-once stdin reader. Avoids pulling in `readline-sync` or `inquirer`. */
function readLine(prompt: string): Promise<string> {
  const rl = require('readline').createInterface({ input: process.stdin, output: process.stdout });
  return new Promise<string>((resolve) => {
    rl.question(prompt, (answer: string) => {
      rl.close();
      resolve(answer);
    });
  });
}

/** Expand a leading `~` to the user's home directory and otherwise resolve
 *  to an absolute path. Needed wherever paths come from `.env` / CLI flags
 *  and are passed to subprocesses (e.g. `solana` via execFileSync), since
 *  there is no shell to expand `~` for us. */
export function expandHome(p: string): string {
  return p.startsWith('~') ? path.join(os.homedir(), p.slice(1)) : path.resolve(p);
}

/** Load a Solana CLI-format keypair (JSON byte array). */
export function loadKeypair(p: string): Keypair {
  const resolved = expandHome(p);
  if (!fs.existsSync(resolved)) throw new Error(`keypair not found at ${resolved}`);
  const raw = JSON.parse(fs.readFileSync(resolved, 'utf8'));
  if (!Array.isArray(raw)) throw new Error(`expected JSON array of bytes at ${resolved}`);
  return Keypair.fromSecretKey(Uint8Array.from(raw));
}

/** Default payer keypair path. Honors `YDELTA_PAYER_KEYPAIR` from `.env`
 *  / process.env, otherwise falls back to the Solana-CLI default. */
export function defaultPayerPath(): string {
  return process.env.YDELTA_PAYER_KEYPAIR ?? path.join(os.homedir(), '.config', 'solana', 'id.json');
}

/** Default cluster — honors `YDELTA_CLUSTER`; otherwise returns `undefined`
 *  so callers can `requireArg(args, 'cluster')` to fail loudly. */
export function defaultCluster(): string | undefined {
  return process.env.YDELTA_CLUSTER;
}

/** Connection factory with the commitment we want for setup scripts (we
 *  always wait for `confirmed` so subsequent ixs see the prior state). */
export function makeConnection(cluster: string, commitment: Commitment = 'confirmed'): Connection {
  return new Connection(resolveRpc(cluster), commitment);
}

/** Parse `--key value` style argv into a flat record. Repeated keys are
 *  joined into arrays (e.g. multiple `--profile foo` flags). */
export function parseArgs(argv: readonly string[]): Record<string, string | string[] | boolean> {
  const out: Record<string, string | string[] | boolean> = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (!a.startsWith('--')) continue;
    const key = a.slice(2);
    const next = argv[i + 1];
    if (!next || next.startsWith('--')) {
      out[key] = true;
      continue;
    }
    const existing = out[key];
    if (existing === undefined) {
      out[key] = next;
    } else if (Array.isArray(existing)) {
      existing.push(next);
    } else {
      out[key] = [String(existing), next];
    }
    i++;
  }
  return out;
}

export function requireArg(args: Record<string, string | string[] | boolean>, key: string): string {
  const v = args[key];
  if (v === undefined || typeof v === 'boolean') throw new Error(`missing required --${key}`);
  if (Array.isArray(v)) return v[v.length - 1];
  return v;
}

export function optionalArg(args: Record<string, string | string[] | boolean>, key: string): string | undefined {
  const v = args[key];
  if (v === undefined || typeof v === 'boolean') return undefined;
  if (Array.isArray(v)) return v[v.length - 1];
  return v;
}

export type SendOpts = {
  /** Confirm the tx before returning; default true. */
  confirm?: boolean;
  /** Skip preflight (use sparingly). */
  skipPreflight?: boolean;
  /** Wait for `finalized` instead of `confirmed`. Use for irreversible one-shot
   *  setup txs on mainnet (CreateGlobalConfig, CreateMarket, CreateVault). */
  finalize?: boolean;
};

/** Send a legacy tx with the given ixs, signed by `payer + signers`. */
export async function sendIxs(
  connection: Connection,
  payer: Keypair,
  ixs: TransactionInstruction[],
  signers: Keypair[] = [],
  opts: SendOpts = {},
): Promise<string> {
  const tx = new Transaction().add(...ixs);
  if (opts.confirm === false) {
    return connection.sendTransaction(tx, [payer, ...signers], {
      skipPreflight: opts.skipPreflight ?? false,
    });
  }
  return sendAndConfirmTransaction(connection, tx, [payer, ...signers], {
    skipPreflight: opts.skipPreflight ?? false,
    commitment: opts.finalize ? 'finalized' : 'confirmed',
  });
}

/** Format a Pubkey for display. */
export function fmtKey(k: PublicKey): string {
  return k.toBase58();
}

/** Ensure the directory of `filepath` exists (mkdir -p). Defaults to mode
 *  0o755 (matches default umask). Pass `mode: 0o700` when the directory
 *  contains files that themselves carry secrets. */
export function mkdirp(filepath: string, opts: { mode?: number } = {}): void {
  fs.mkdirSync(path.dirname(filepath), { recursive: true, mode: opts.mode });
}

/** Write JSON with a trailing newline and a header note. */
export function writeJson(filepath: string, body: unknown): void {
  mkdirp(filepath);
  fs.writeFileSync(filepath, JSON.stringify(body, null, 2) + '\n');
  console.log(`wrote ${path.relative(process.cwd(), filepath)} (${fs.statSync(filepath).size} bytes)`);
}

export function readJsonIfExists<T>(filepath: string): T | null {
  if (!fs.existsSync(filepath)) return null;
  return JSON.parse(fs.readFileSync(filepath, 'utf8')) as T;
}

/** Repo-root absolute path. */
export const REPO_ROOT = path.resolve(__dirname, '..', '..');

/** `.local/` is gitignored — safe place for cluster-specific artifacts. */
export const LOCAL_DIR = path.join(REPO_ROOT, '.local');

/** Encode a Keypair's secret key as the Solana-CLI JSON byte-array format. */
export function encodeKeypairToCliJson(kp: Keypair): number[] {
  return Array.from(kp.secretKey);
}

/** Sanitize user / cluster identifiers for local filenames. */
export function safeFileSegment(input: string): string {
  const cleaned = input.replace(/[^a-zA-Z0-9._-]+/g, '_').replace(/^_+|_+$/g, '');
  return cleaned.length > 0 ? cleaned : 'default';
}

/** Print a section header. */
export function header(title: string): void {
  console.log(`\n━━━ ${title} ━━━`);
}
