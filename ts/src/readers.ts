/**
 * State-readers — small Connection-bound helpers for the SDK's UI / keeper
 * surface area. These run over the zero-copy decoders in `ts/src/accounts/`
 * and the marginfi v0.1.8 reader in `ts/src/marginfi.ts`.
 */
import { Connection, PublicKey } from '@solana/web3.js';

import { decodeGlobalVault, GlobalVault } from './accounts/index.js';
import { decodeMarket, Market, MarketHeader } from './accounts/market.js';
import { SubVault, SubVaultOrderRef } from './accounts/subVault.js';
import { globalVaultPda } from './pdas.js';

/** Fetch + decode a market. */
export async function fetchMarket(conn: Connection, market: PublicKey): Promise<Market | null> {
  const acc = await conn.getAccountInfo(market);
  if (!acc) return null;
  return decodeMarket(acc.data);
}

/** Fetch + decode the market header only (cheaper if you don't need the trees). */
export async function fetchMarketHeader(
  conn: Connection,
  market: PublicKey,
): Promise<MarketHeader | null> {
  const acc = await conn.getAccountInfo(market);
  if (!acc) return null;
  const { decodeMarketHeader } = await import('./accounts/market.js');
  return decodeMarketHeader(acc.data);
}

/**
 * Fetch + decode the GlobalVault keyed to a marginfi bank. The vault PDA is
 * `[b"vault", bank]`, so pass the bank (a market's `debtLendingPool`), not a mint.
 */
export async function fetchVault(
  conn: Connection,
  bank: PublicKey,
): Promise<GlobalVault | null> {
  const [vault] = globalVaultPda(bank);
  const acc = await conn.getAccountInfo(vault);
  if (!acc) return null;
  return decodeGlobalVault(acc.data);
}

/** Find a `SubVault` by `(vault, sub_vault_id)`. Pass the bank the vault is keyed to. */
export async function fetchSubVault(
  conn: Connection,
  bank: PublicKey,
  subVaultId: number,
): Promise<SubVault | null> {
  const v = await fetchVault(conn, bank);
  if (!v) return null;
  const entry = v.subVaults.find((s) => s.subVault.subVaultId === subVaultId);
  return entry?.subVault ?? null;
}

/** List every `SubVault` whose `curator` matches the given pubkey. Pass the bank the vault is keyed to. */
export async function listSubVaultsForCurator(
  conn: Connection,
  bank: PublicKey,
  curator: PublicKey,
): Promise<SubVault[]> {
  const v = await fetchVault(conn, bank);
  if (!v) return [];
  return v.subVaults
    .map((s) => s.subVault)
    .filter((s) => s.curator.equals(curator) || s.pendingCurator.equals(curator));
}

/** List every market where `subVaultId` has a live ask. Pass the bank the vault is keyed to. */
export async function listMarketsForSubVault(
  conn: Connection,
  bank: PublicKey,
  subVaultId: number,
): Promise<SubVaultOrderRef[]> {
  const v = await fetchVault(conn, bank);
  if (!v) return [];
  return v.marketOrders
    .map((m) => m.order)
    .filter((o) => o.subVaultId === subVaultId);
}
