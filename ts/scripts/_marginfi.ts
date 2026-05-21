/**
 * Static marginfi v0.1.8 bank registry.
 *
 * `.local/mainnet-banks.json` is the ONLY source of truth for which
 * marginfi banks our scripts target. No runtime calls to bankCache or
 * any other API — every value (bank pubkey, group, liquidity vault,
 * oracle key, pyth feed-id, etc.) is operator-pinned in the JSON. We
 * still cross-check the on-chain bank when we read it (via
 * `verifyBankMatchesRegistry`) so the JSON can't silently lie about
 * `liquidityVault` or `oracleKey`.
 *
 * assetTag semantics (per the API doc): 3=Kamino, 4=Drift, 5=Solend,
 * 6=JupLend; anything else is native-P0 and eligible. We do not enforce
 * `isNativeP0` here because the operator owns the registry — they wrote
 * it, so anything in it is "approved".
 */
import * as fs from 'node:fs';
import { Connection, PublicKey } from '@solana/web3.js';

import { Bank, decodeBank, OracleSetup } from '../src/marginfi.js';
import { localPath } from './_runner.js';

export const MARGINFI_PROGRAM_ID = new PublicKey('MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA');

/** Shape of a single bank entry in `.local/mainnet-banks.json`. */
export interface BankRegistryEntry {
  tokenSymbol: string;
  mint: string;
  mintDecimals: number;
  bank: string;
  liquidityVault: string;
  assetTag: number;
  oracleSetup: string;
  oracleKey: string;
  pythFeedIdHex?: string;
  pythShardId?: number;
  _note?: string;
}

export interface BankRegistry {
  marginfiProgramId: string;
  marginfiGroup: string;
  banks: BankRegistryEntry[];
}

let cached: BankRegistry | null = null;

/**
 * Load + memoise the static bank registry from `.local/mainnet-banks.json`.
 * Throws with a copy-from-example hint if the operator forgot to seed it.
 */
export function loadBankRegistry(): BankRegistry {
  if (cached) return cached;
  const file = localPath('mainnet-banks.json');
  if (!fs.existsSync(file)) {
    throw new Error(
      `missing ${file}. Copy from .local.example/mainnet-banks.json and verify each ` +
      `entry against \`solana account <bank>\` before running on mainnet.`,
    );
  }
  const raw = fs.readFileSync(file, 'utf8');
  const parsed = JSON.parse(raw) as BankRegistry;
  if (!parsed.marginfiGroup || !Array.isArray(parsed.banks)) {
    throw new Error(`${file}: malformed — expected { marginfiGroup, banks: [...] }`);
  }
  if (parsed.marginfiProgramId && parsed.marginfiProgramId !== MARGINFI_PROGRAM_ID.toBase58()) {
    throw new Error(
      `${file}: marginfiProgramId ${parsed.marginfiProgramId} ≠ pinned v0.1.8 ` +
      `program id ${MARGINFI_PROGRAM_ID.toBase58()}. Bump only if you've verified the ` +
      `new program is the v0.1.8 fixture yDelta targets.`,
    );
  }
  cached = parsed;
  return parsed;
}

/**
 * Resolve a bank registry entry by debt-mint string. Throws if missing or
 * duplicate. The operator-curated registry is expected to be 1-1 from
 * mint → bank; a duplicate means the registry needs disambiguation.
 */
export function resolveBankByMint(mint: PublicKey | string): BankRegistryEntry {
  const reg = loadBankRegistry();
  const want = typeof mint === 'string' ? mint : mint.toBase58();
  const hits = reg.banks.filter((b) => b.mint === want);
  if (hits.length === 0) {
    throw new Error(
      `mainnet-banks.json: no entry for mint ${want}. Add it to the registry.`,
    );
  }
  if (hits.length > 1) {
    const banks = hits.map((h) => `${h.tokenSymbol}:${h.bank}`).join(', ');
    throw new Error(
      `mainnet-banks.json: ${hits.length} entries for mint ${want} (${banks}). ` +
      `One mint → one bank; remove the extras.`,
    );
  }
  return hits[0];
}

/** Read + decode a marginfi v0.1.8 `Bank` account directly from chain. */
export async function readBankOnchain(conn: Connection, bank: PublicKey): Promise<Bank> {
  const info = await conn.getAccountInfo(bank, 'confirmed');
  if (!info) throw new Error(`bank ${bank.toBase58()} not found on chain`);
  if (!info.owner.equals(MARGINFI_PROGRAM_ID)) {
    throw new Error(
      `bank ${bank.toBase58()} not owned by marginfi (${info.owner.toBase58()})`,
    );
  }
  return decodeBank(info.data);
}

/**
 * Crosscheck a registry entry against on-chain bank state. Fail fast on
 * any drift — the registry is the operator's claim about mainnet; any
 * mismatch means the registry is wrong or mainnet rotated and we should
 * stop before sending real txs.
 */
export function verifyBankMatchesRegistry(
  bank: Bank,
  entry: BankRegistryEntry,
  expectedGroup: PublicKey,
): void {
  if (!bank.group.equals(expectedGroup)) {
    throw new Error(
      `bank ${entry.bank} (${entry.tokenSymbol}) group ${bank.group.toBase58()} ` +
      `≠ registry.marginfiGroup ${expectedGroup.toBase58()}`,
    );
  }
  if (bank.mint.toBase58() !== entry.mint) {
    throw new Error(
      `bank ${entry.bank} (${entry.tokenSymbol}) mint ${bank.mint.toBase58()} ` +
      `≠ registry.mint ${entry.mint}`,
    );
  }
  if (bank.liquidityVault.toBase58() !== entry.liquidityVault) {
    throw new Error(
      `bank ${entry.bank} (${entry.tokenSymbol}) liquidityVault ` +
      `${bank.liquidityVault.toBase58()} ≠ registry ${entry.liquidityVault}`,
    );
  }
  if (bank.oracleKeys[0].toBase58() !== entry.oracleKey) {
    throw new Error(
      `bank ${entry.bank} (${entry.tokenSymbol}) primary oracle ` +
      `${bank.oracleKeys[0].toBase58()} ≠ registry ${entry.oracleKey}`,
    );
  }
  const expectedSetup = OracleSetup[bank.oracleSetup];
  if (expectedSetup !== entry.oracleSetup) {
    throw new Error(
      `bank ${entry.bank} (${entry.tokenSymbol}) oracleSetup ${expectedSetup} ` +
      `≠ registry ${entry.oracleSetup}`,
    );
  }
}

/**
 * Effective oracle accounts the program will read for `bank`. Returns the
 * primary oracle slot plus any auxiliary slots required by the bank's
 * `OracleSetup` (e.g. `StakedWithPythPush` reads 3 accounts).
 */
export function bankOracleAccounts(bank: Bank): PublicKey[] {
  const count = expectedOracleCount(bank.oracleSetup);
  return bank.oracleKeys.slice(0, count);
}

export function expectedOracleCount(setup: OracleSetup): number {
  switch (setup) {
    case OracleSetup.None:
      return 0;
    case OracleSetup.PythPushOracle:
    case OracleSetup.SwitchboardPull:
      return 1;
    case OracleSetup.StakedWithPythPush:
      return 3;
    default:
      return 1;
  }
}

/** PDA `[b"liquidity_vault_auth", bank]` under marginfi. */
export function bankLiquidityVaultAuthority(bank: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('liquidity_vault_auth'), bank.toBuffer()],
    MARGINFI_PROGRAM_ID,
  )[0];
}
