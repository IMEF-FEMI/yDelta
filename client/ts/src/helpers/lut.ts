import {
  AddressLookupTableAccount,
  AddressLookupTableProgram,
  Connection,
  Keypair,
  PublicKey,
  Signer,
  SystemProgram,
  Transaction,
  TransactionInstruction,
  sendAndConfirmTransaction,
} from '@solana/web3.js';
import {
  TOKEN_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
} from '@solana/spl-token';
import { YDELTA_PROGRAM_ID } from '../utils/programId';
import { vaultStagingPda } from '../utils/pdas';
import { Market } from '../market';
import { Vault } from '../vault';

/** Solana caps a single `extendLookupTable` ix at ~30 keys (tx-size limit). */
export const EXTEND_LUT_CHUNK_SIZE = 30;

/** The stable per-market keys worth pinning in a canonical LUT — every order /
 *  deposit / withdraw / loan-lifecycle ix touches some subset of these. Caller
 *  supplies the bank oracle keys (variadic per `OracleSetup`). */
export function canonicalMarketLutKeys(
  market: Market,
  opts: {
    debtOracles?: PublicKey[];
    collateralOracles?: PublicKey[];
    bankLiquidityVaults?: PublicKey[];
    bankLiquidityVaultAuthorities?: PublicKey[];
    marginfiProgram: PublicKey;
    programId?: PublicKey;
  },
): PublicKey[] {
  const programId = opts.programId ?? YDELTA_PROGRAM_ID;
  const header = market.header();
  const keys: PublicKey[] = [
    market.address,
    market.marketSigner(),
    header.lenderIntegrationAccount,
    header.borrowerIntegrationAccount,
    market.debtMint(),
    market.collateralMint(),
    header.debtVault,
    header.collateralVault,
    market.marginfiGroup(),
    market.debtBank(),
    market.collateralBank(),
    opts.marginfiProgram,
    programId,
    SystemProgram.programId,
    TOKEN_PROGRAM_ID,
    TOKEN_2022_PROGRAM_ID,
  ];
  for (const o of opts.debtOracles ?? []) keys.push(o);
  for (const o of opts.collateralOracles ?? []) keys.push(o);
  for (const v of opts.bankLiquidityVaults ?? []) keys.push(v);
  for (const a of opts.bankLiquidityVaultAuthorities ?? []) keys.push(a);
  return dedupKeys(keys);
}

export function canonicalVaultLutKeys(
  vault: Vault,
  opts: {
    bankOracles?: PublicKey[];
    bankLiquidityVault?: PublicKey;
    bankLiquidityVaultAuthority?: PublicKey;
    marginfiProgram: PublicKey;
    programId?: PublicKey;
  },
): PublicKey[] {
  const programId = opts.programId ?? YDELTA_PROGRAM_ID;
  const header = vault.header();
  const keys: PublicKey[] = [
    vault.address,
    header.globalVaultSigner,
    header.integrationAccount,
    vaultStagingPda(vault.address, programId)[0],
    vault.mint(),
    vault.lendingPool(),
    header.integrationPool,
    opts.marginfiProgram,
    programId,
    SystemProgram.programId,
    TOKEN_PROGRAM_ID,
    TOKEN_2022_PROGRAM_ID,
  ];
  if (opts.bankLiquidityVault) keys.push(opts.bankLiquidityVault);
  if (opts.bankLiquidityVaultAuthority) keys.push(opts.bankLiquidityVaultAuthority);
  for (const o of opts.bankOracles ?? []) keys.push(o);
  return dedupKeys(keys);
}

/** Deduplicate while preserving insertion order. */
export function dedupKeys(keys: PublicKey[]): PublicKey[] {
  const seen = new Set<string>();
  const out: PublicKey[] = [];
  for (const k of keys) {
    const s = k.toBase58();
    if (seen.has(s)) continue;
    seen.add(s);
    out.push(k);
  }
  return out;
}

/** Collect every distinct account pubkey across a set of instructions
 *  (including each ix's `programId`). Useful for ad-hoc LUT creation. */
export function uniqueKeysFromIxs(ixs: TransactionInstruction[]): PublicKey[] {
  const collected: PublicKey[] = [];
  for (const ix of ixs) {
    collected.push(ix.programId);
    for (const k of ix.keys) collected.push(k.pubkey);
  }
  return dedupKeys(collected);
}

/** Slice an array of keys into chunks of `chunkSize` for the extend-LUT loop. */
export function chunkKeys(keys: PublicKey[], chunkSize: number = EXTEND_LUT_CHUNK_SIZE): PublicKey[][] {
  if (chunkSize <= 0) throw new Error('chunkSize must be positive');
  const chunks: PublicKey[][] = [];
  for (let i = 0; i < keys.length; i += chunkSize) {
    chunks.push(keys.slice(i, i + chunkSize));
  }
  return chunks;
}

export type CreateLutResult = {
  /** The LUT address. */
  lookupTable: PublicKey;
  /** The instructions that were sent (createIx + N extendIxs). */
  instructions: TransactionInstruction[];
  /** Tx signatures, in send order. */
  signatures: string[];
};

/** Create a fresh LUT and extend it with `keys`. Each extend tx pushes at
 *  most `EXTEND_LUT_CHUNK_SIZE` keys to stay under the 1232-byte tx limit. */
export async function createAndExtendLut({
  connection,
  authority,
  payer,
  keys,
  recentSlot,
}: {
  connection: Connection;
  authority: Keypair;
  payer: Keypair;
  keys: PublicKey[];
  /** Slot used to derive the LUT address. When omitted, the SDK fetches one
   *  immediately before creation. */
  recentSlot?: number;
}): Promise<CreateLutResult> {
  const slot = recentSlot ?? (await connection.getSlot('finalized'));
  const [createIx, lookupTable] = AddressLookupTableProgram.createLookupTable({
    authority: authority.publicKey,
    payer: payer.publicKey,
    recentSlot: slot,
  });

  const signatures: string[] = [];
  const instructions: TransactionInstruction[] = [createIx];

  // First tx: createLookupTable + first chunk (if any).
  const allChunks = chunkKeys(dedupKeys(keys));
  const firstChunk = allChunks.shift();
  const firstIxs: TransactionInstruction[] = [createIx];
  if (firstChunk && firstChunk.length > 0) {
    const ext = AddressLookupTableProgram.extendLookupTable({
      lookupTable,
      authority: authority.publicKey,
      payer: payer.publicKey,
      addresses: firstChunk,
    });
    firstIxs.push(ext);
    instructions.push(ext);
  }
  signatures.push(await sendTx(connection, payer, firstIxs, [authority]));

  // Subsequent extends.
  for (const chunk of allChunks) {
    const ext = AddressLookupTableProgram.extendLookupTable({
      lookupTable,
      authority: authority.publicKey,
      payer: payer.publicKey,
      addresses: chunk,
    });
    instructions.push(ext);
    signatures.push(await sendTx(connection, payer, [ext], [authority]));
  }

  return { lookupTable, instructions, signatures };
}

async function sendTx(
  connection: Connection,
  payer: Keypair,
  ixs: TransactionInstruction[],
  signers: Signer[],
): Promise<string> {
  const tx = new Transaction().add(...ixs);
  return sendAndConfirmTransaction(connection, tx, [payer, ...signers], {
    commitment: 'confirmed',
  });
}

/** Fetch an `AddressLookupTableAccount` by address. Returns `null` when the
 *  account doesn't exist (e.g. authority closed it). */
export async function fetchLookupTable(
  connection: Connection,
  lookupTable: PublicKey,
): Promise<AddressLookupTableAccount | null> {
  const res = await connection.getAddressLookupTable(lookupTable);
  return res.value ?? null;
}

/** Compute the keys in `desired` that are NOT already in `existing`. Useful
 *  to incrementally extend a LUT when new accounts (oracles, banks) come online. */
export function lutDeltaKeys(
  existing: AddressLookupTableAccount,
  desired: PublicKey[],
): PublicKey[] {
  const have = new Set(existing.state.addresses.map((a) => a.toBase58()));
  return dedupKeys(desired).filter((k) => !have.has(k.toBase58()));
}

/** Extend an existing LUT to cover all keys in `desired` (idempotent). Skips
 *  the no-op send when the LUT already contains every key. */
export async function extendLutIfNeeded({
  connection,
  lookupTable,
  authority,
  payer,
  desired,
}: {
  connection: Connection;
  lookupTable: PublicKey;
  authority: Keypair;
  payer: Keypair;
  desired: PublicKey[];
}): Promise<{ signatures: string[]; addedKeys: PublicKey[] }> {
  const existing = await fetchLookupTable(connection, lookupTable);
  if (!existing) throw new Error(`Lookup table ${lookupTable.toBase58()} not found`);
  const delta = lutDeltaKeys(existing, desired);
  if (delta.length === 0) return { signatures: [], addedKeys: [] };
  const signatures: string[] = [];
  for (const chunk of chunkKeys(delta)) {
    const ext = AddressLookupTableProgram.extendLookupTable({
      lookupTable,
      authority: authority.publicKey,
      payer: payer.publicKey,
      addresses: chunk,
    });
    signatures.push(await sendTx(connection, payer, [ext], [authority]));
  }
  return { signatures, addedKeys: delta };
}
