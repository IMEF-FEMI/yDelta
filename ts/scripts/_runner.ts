/**
 * Shared bootstrap helpers for the TS action scripts.
 *
 * Every script under `ts/scripts/*.ts` is a thin "read inputs from
 * `.local/<name>.json` → build ix(es) → send tx → write result back" wrapper.
 * The helpers here cover the parts that don't change between scripts:
 *
 *   - `.env` loading (dotenv) with sensible defaults.
 *   - `Connection` + signer-keypair loading.
 *   - `.local/` JSON read/write with atomic-rename writes.
 *   - Tx sender with confirmation + a single retry on blockhash expiry.
 *
 * No CLI argument parsing — scripts read everything from `.local/*.json`
 * so a run is reproducible and inspectable. Mutations (new pubkeys, tx
 * sigs) are written back to the same JSON tree.
 */
import {
  AddressLookupTableAccount,
  Connection,
  Keypair,
  PublicKey,
  Signer,
  SystemProgram,
  Transaction,
  TransactionInstruction,
  TransactionMessage,
  VersionedTransaction,
} from '@solana/web3.js';
import {
  createAssociatedTokenAccountIdempotentInstruction,
  createSyncNativeInstruction,
  getAssociatedTokenAddressSync,
  NATIVE_MINT,
  TOKEN_PROGRAM_ID,
} from '@solana/spl-token';
import * as fs from 'node:fs';
import * as path from 'node:path';
import bs58 from 'bs58';
import dotenv from 'dotenv';

dotenv.config();

const DEFAULT_RPC = 'https://api.mainnet-beta.solana.com';
const DEFAULT_HERMES = 'https://hermes.pyth.network';

export const REPO_ROOT = path.resolve(new URL('../..', import.meta.url).pathname);

export function localDir(): string {
  const override = process.env.YDELTA_LOCAL_DIR;
  return override && override.length > 0 ? override : path.join(REPO_ROOT, '.local');
}

export function localPath(name: string): string {
  return path.join(localDir(), name);
}

export function loadConnection(): Connection {
  const url = process.env.YDELTA_RPC_URL ?? DEFAULT_RPC;
  return new Connection(url, 'confirmed');
}

export function hermesUrl(): string {
  return process.env.YDELTA_HERMES_URL ?? DEFAULT_HERMES;
}

export function loadSigner(): Keypair {
  const explicit =
    process.env.YDELTA_DEPLOYER_KEYPAIR_PATH ??
    process.env.YDELTA_KEYPAIR_PATH;
  const raw = explicit && explicit.length > 0
    ? explicit
    : path.join(process.env.HOME ?? '', '.config/solana/id.json');
  const candidate = expandTilde(raw);
  if (!fs.existsSync(candidate)) {
    throw new Error(
      `signer keypair not found at ${candidate}. ` +
      `Set YDELTA_DEPLOYER_KEYPAIR_PATH / YDELTA_KEYPAIR_PATH or create ~/.config/solana/id.json`,
    );
  }
  return readKeypairFile(candidate);
}

function expandTilde(p: string): string {
  // dotenv preserves `~` literally; Node's fs doesn't expand it. Resolve
  // here so operators can keep the shell-friendly `~/.config/solana/id.json`
  // syntax in `.env`.
  if (p === '~') return process.env.HOME ?? p;
  if (p.startsWith('~/')) return path.join(process.env.HOME ?? '', p.slice(2));
  return p;
}

export function readKeypairFile(file: string): Keypair {
  const raw = fs.readFileSync(file, 'utf8').trim();
  const bytes = JSON.parse(raw);
  if (!Array.isArray(bytes)) {
    throw new Error(`${file}: keypair file must be a JSON array of bytes`);
  }
  return Keypair.fromSecretKey(Uint8Array.from(bytes));
}

export function readKeypairFromBase58(secretBase58: string): Keypair {
  return Keypair.fromSecretKey(bs58.decode(secretBase58));
}

/** Load a role keypair from a base58 secret-key env var. */
function loadKeypairFromEnvB58(envVar: string, role: string): Keypair {
  const raw = (process.env[envVar] ?? '').trim();
  if (raw.length === 0) {
    throw new Error(`${role} keypair not set — define ${envVar}=<base58 secret key> in .env`);
  }
  return readKeypairFromBase58(raw);
}

/** LP depositor for the global-vault scripts (`YDELTA_DEPOSITOR_KEYPAIR_B58`). */
export function loadDepositor(): Keypair {
  return loadKeypairFromEnvB58('YDELTA_DEPOSITOR_KEYPAIR_B58', 'depositor');
}

/** Borrower for the market-seat scripts (`YDELTA_BORROWER_KEYPAIR_B58`). */
export function loadBorrower(): Keypair {
  return loadKeypairFromEnvB58('YDELTA_BORROWER_KEYPAIR_B58', 'borrower');
}

/** Canonical associated-token-account address for `owner`/`mint`. */
export function ataFor(
  owner: PublicKey,
  mint: PublicKey,
  tokenProgram: PublicKey = TOKEN_PROGRAM_ID,
): PublicKey {
  return getAssociatedTokenAddressSync(mint, owner, false, tokenProgram);
}

/**
 * Idempotent-create ix for `owner`'s ATA — a no-op on-chain if it already
 * exists. Scripts derive the ATA (never take it as input) and prepend this so
 * a missing token account is created in the same tx rather than failing.
 */
export function createAtaIdempotentIx(
  payer: PublicKey,
  owner: PublicKey,
  mint: PublicKey,
  tokenProgram: PublicKey = TOKEN_PROGRAM_ID,
): TransactionInstruction {
  return createAssociatedTokenAccountIdempotentInstruction(
    payer,
    ataFor(owner, mint, tokenProgram),
    owner,
    mint,
    tokenProgram,
  );
}

/**
 * Wrap `amountLamports` of native SOL into `owner`'s wSOL ATA: fund the ATA
 * then `syncNative`. Returns [] for non-wSOL mints. Used so a borrower can
 * deposit SOL collateral without a separate manual wrap step.
 */
export function wrapSolIxs(
  owner: PublicKey,
  mint: PublicKey,
  amountLamports: bigint,
): TransactionInstruction[] {
  if (!mint.equals(NATIVE_MINT) || amountLamports === 0n) return [];
  const ata = ataFor(owner, mint);
  return [
    SystemProgram.transfer({ fromPubkey: owner, toPubkey: ata, lamports: amountLamports }),
    createSyncNativeInstruction(ata),
  ];
}

export function ensureLocalDir(): void {
  fs.mkdirSync(localDir(), { recursive: true });
}

export function readJson<T>(filename: string): T {
  const file = localPath(filename);
  if (!fs.existsSync(file)) {
    throw new Error(`missing .local file: ${file}`);
  }
  return JSON.parse(fs.readFileSync(file, 'utf8')) as T;
}

export function readJsonOptional<T>(filename: string): T | null {
  const file = localPath(filename);
  if (!fs.existsSync(file)) return null;
  return JSON.parse(fs.readFileSync(file, 'utf8')) as T;
}

export function writeJson(filename: string, payload: unknown): void {
  ensureLocalDir();
  const file = localPath(filename);
  const tmp = `${file}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(payload, null, 2) + '\n', 'utf8');
  fs.renameSync(tmp, file);
}

export function loadGlobalConfig(): { protocolAdmin: string; pda: string; signature?: string } {
  return readJson('global-config.json');
}

export function loadProtocol(): { programId: string; programDataPda: string; deployer: string } {
  return readJson('protocol.json');
}

export interface SendTxOptions {
  skipPreflight?: boolean;
  /** Soft commitment to wait for. Default `confirmed`. */
  commitment?: 'processed' | 'confirmed' | 'finalized';
}

/**
 * Refuse to broadcast against a mainnet RPC unless the operator has
 * explicitly opted in via `YDELTA_CONFIRM`. Guards against a misconfigured
 * `.local/*.json` firing a real fund-moving tx by accident.
 */
export function assertSendAllowed(conn: Connection): void {
  if (!/mainnet/i.test(conn.rpcEndpoint ?? '')) return;
  const confirm = (process.env.YDELTA_CONFIRM ?? '').toLowerCase();
  if (confirm !== '1' && confirm !== 'true' && confirm !== 'yes') {
    throw new Error(
      `Refusing to broadcast a MAINNET transaction (${conn.rpcEndpoint}) without ` +
      `explicit confirmation. Double-check your .local/*.json inputs, then set ` +
      `YDELTA_CONFIRM=1 to proceed.`,
    );
  }
}

/**
 * Build + sign + send a v1 legacy transaction, then wait for the chosen
 * commitment. Throws on simulation failure (logs are surfaced).
 */
export async function sendIxs(
  conn: Connection,
  payer: Signer,
  ixs: TransactionInstruction[],
  signers: Signer[] = [],
  opts: SendTxOptions = {},
): Promise<string> {
  assertSendAllowed(conn);
  const tx = new Transaction();
  for (const ix of ixs) tx.add(ix);
  const { blockhash, lastValidBlockHeight } = await conn.getLatestBlockhash('confirmed');
  tx.recentBlockhash = blockhash;
  tx.feePayer = payer.publicKey;
  const allSigners: Signer[] = [payer, ...signers];
  tx.sign(...dedupeSigners(allSigners));

  const raw = tx.serialize();
  const sig = await conn.sendRawTransaction(raw, {
    skipPreflight: opts.skipPreflight ?? false,
    preflightCommitment: opts.commitment ?? 'confirmed',
  });
  const conf = await conn.confirmTransaction(
    { signature: sig, blockhash, lastValidBlockHeight },
    opts.commitment ?? 'confirmed',
  );
  if (conf.value.err) {
    throw new Error(`tx ${sig} failed: ${JSON.stringify(conf.value.err)}`);
  }
  return sig;
}

/**
 * Build + sign + send a v0 (versioned) transaction with optional address
 * lookup tables, then wait for confirmation. Used to bundle a Switchboard
 * oracle update with the consuming ix in a single atomic tx — the LUTs
 * compress the consumer's account list under the 1232-byte tx limit.
 */
export async function sendV0Ixs(
  conn: Connection,
  payer: Signer,
  ixs: TransactionInstruction[],
  opts: {
    luts?: AddressLookupTableAccount[];
    extraSigners?: Signer[];
    commitment?: 'processed' | 'confirmed' | 'finalized';
  } = {},
): Promise<string> {
  assertSendAllowed(conn);
  const { blockhash, lastValidBlockHeight } = await conn.getLatestBlockhash('confirmed');
  const msg = new TransactionMessage({
    payerKey: payer.publicKey,
    recentBlockhash: blockhash,
    instructions: ixs,
  }).compileToV0Message(opts.luts ?? []);
  const tx = new VersionedTransaction(msg);
  tx.sign(dedupeSigners([payer, ...(opts.extraSigners ?? [])]));
  const sig = await conn.sendRawTransaction(tx.serialize(), {
    preflightCommitment: opts.commitment ?? 'confirmed',
  });
  const conf = await conn.confirmTransaction(
    { signature: sig, blockhash, lastValidBlockHeight },
    opts.commitment ?? 'confirmed',
  );
  if (conf.value.err) {
    throw new Error(`tx ${sig} failed: ${JSON.stringify(conf.value.err)}`);
  }
  return sig;
}

function dedupeSigners(signers: Signer[]): Signer[] {
  const seen = new Set<string>();
  const out: Signer[] = [];
  for (const s of signers) {
    const key = s.publicKey.toBase58();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(s);
  }
  return out;
}

/**
 * Append a record of work this script just performed to `.local/tx-log.json`.
 * Every signed-tx-emitting script should call this exactly once just before
 * exit so the operator has a single ledger to chase from.
 */
export interface TxLogEntry {
  script: string;
  signatures: string[];
  oracleCrank?: { provider: string; oracle: string; signatures: string[] }[];
  summary: Record<string, unknown>;
  timestamp: string;
}

export function appendTxLog(entry: Omit<TxLogEntry, 'timestamp'>): void {
  ensureLocalDir();
  const file = localPath('tx-log.json');
  let existing: TxLogEntry[] = [];
  if (fs.existsSync(file)) {
    try {
      existing = JSON.parse(fs.readFileSync(file, 'utf8'));
      if (!Array.isArray(existing)) existing = [];
    } catch {
      existing = [];
    }
  }
  existing.push({ ...entry, timestamp: new Date().toISOString() });
  fs.writeFileSync(file, JSON.stringify(existing, null, 2) + '\n', 'utf8');
}

/** Convenience pretty-printer for a pubkey, accepts string-or-PublicKey. */
export function pk(v: PublicKey | string): string {
  return typeof v === 'string' ? v : v.toBase58();
}

export function toPubkey(v: PublicKey | string): PublicKey {
  return typeof v === 'string' ? new PublicKey(v) : v;
}

export function log(...args: unknown[]): void {
  console.log(...args);
}
