/**
 * Shared scaffolding for the `ts/scripts/*` operational scripts.
 *
 * Each script:
 *   1. Parses argv via a tiny `flag()` reader (no external CLI library).
 *   2. Loads an RPC + signer keypair from env (`YDELTA_RPC_URL`,
 *      `YDELTA_KEYPAIR_PATH`).
 *   3. Composes its instructions — usually `[cuBudgetIx?, ...ydeltaIxs]`.
 *   4. Either prints the simulated ix plan (`--dry-run`) or sends the tx
 *      and waits for confirmation.
 */
import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { resolve } from 'node:path';

import {
  ComputeBudgetProgram,
  Connection,
  Keypair,
  PublicKey,
  TransactionInstruction,
  TransactionMessage,
  VersionedTransaction,
} from '@solana/web3.js';

/** Read a `--name value` style flag. Returns `undefined` if absent. */
export function flag(name: string): string | undefined {
  const argv = process.argv.slice(2);
  const idx = argv.findIndex((a) => a === `--${name}`);
  if (idx === -1) return undefined;
  return argv[idx + 1];
}

/** Read a boolean `--name` switch. */
export function hasFlag(name: string): boolean {
  return process.argv.slice(2).some((a) => a === `--${name}`);
}

export function requireFlag(name: string): string {
  const v = flag(name);
  if (v === undefined) {
    console.error(`error: --${name} is required`);
    process.exit(1);
  }
  return v;
}

export function pubkeyFlag(name: string): PublicKey {
  return new PublicKey(requireFlag(name));
}

export function optionalPubkeyFlag(name: string): PublicKey | undefined {
  const v = flag(name);
  return v ? new PublicKey(v) : undefined;
}

export function bigintFlag(name: string): bigint {
  return BigInt(requireFlag(name));
}

export function numberFlag(name: string): number {
  return Number(requireFlag(name));
}

/** Default RPC = mainnet-beta. Override with `YDELTA_RPC_URL`. */
export function loadConnection(): Connection {
  const url = process.env.YDELTA_RPC_URL ?? 'https://api.mainnet-beta.solana.com';
  return new Connection(url, 'confirmed');
}

/**
 * Load a signer keypair. Searches:
 *   1. `--keypair <path>` flag
 *   2. `YDELTA_KEYPAIR_PATH` env var
 *   3. `~/.config/solana/id.json`
 */
export function loadSigner(): Keypair {
  const path =
    flag('keypair') ??
    process.env.YDELTA_KEYPAIR_PATH ??
    resolve(homedir(), '.config/solana/id.json');
  const json = JSON.parse(readFileSync(path, 'utf-8'));
  return Keypair.fromSecretKey(Uint8Array.from(json));
}

/** Pretty-print an ix plan for dry-run mode. */
export function printIxPlan(ixs: TransactionInstruction[]): void {
  console.log(`Plan: ${ixs.length} instruction(s)`);
  for (let i = 0; i < ixs.length; i++) {
    const ix = ixs[i];
    console.log(`  [${i}] program=${ix.programId.toBase58()} keys=${ix.keys.length} data=${ix.data.length}B`);
    for (let k = 0; k < ix.keys.length; k++) {
      const m = ix.keys[k];
      const flagStr = `${m.isSigner ? 's' : '-'}${m.isWritable ? 'w' : '-'}`;
      console.log(`        ${flagStr}  ${m.pubkey.toBase58()}`);
    }
  }
}

/**
 * Send a single-tx bundle: prepend an optional CU-limit ix, build the v0
 * tx, sign with `signer`, send + confirm. Returns the signature.
 */
export async function sendIxs(
  conn: Connection,
  signer: Keypair,
  ixs: TransactionInstruction[],
  opts: { cuLimit?: number; extraSigners?: Keypair[] } = {},
): Promise<string> {
  const prelude: TransactionInstruction[] = [];
  if (opts.cuLimit) {
    prelude.push(ComputeBudgetProgram.setComputeUnitLimit({ units: opts.cuLimit }));
  }
  const blockhash = (await conn.getLatestBlockhash()).blockhash;
  const message = new TransactionMessage({
    payerKey: signer.publicKey,
    recentBlockhash: blockhash,
    instructions: [...prelude, ...ixs],
  }).compileToV0Message();
  const tx = new VersionedTransaction(message);
  tx.sign([signer, ...(opts.extraSigners ?? [])]);
  const sig = await conn.sendTransaction(tx);
  await conn.confirmTransaction(sig, 'confirmed');
  return sig;
}

/** Driver: dry-run prints, otherwise sends. */
export async function runScript(args: {
  conn: Connection;
  signer: Keypair;
  ixs: TransactionInstruction[];
  cuLimit?: number;
  extraSigners?: Keypair[];
}): Promise<void> {
  if (hasFlag('dry-run')) {
    printIxPlan(args.ixs);
    return;
  }
  const sig = await sendIxs(args.conn, args.signer, args.ixs, {
    cuLimit: args.cuLimit,
    extraSigners: args.extraSigners,
  });
  console.log(`sent: ${sig}`);
}
