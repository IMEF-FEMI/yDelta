/**
 * State-readers — small Connection-bound helpers for the SDK's UI / keeper
 * surface area. These run over the zero-copy decoders in `ts/src/accounts/`
 * and the marginfi v0.1.8 reader in `ts/src/marginfi.ts`.
 */
import { Connection, PublicKey } from '@solana/web3.js';

import { decodeGlobalVault, GlobalVault } from './accounts/index.js';
import { decodeMarket, Market, MarketHeader } from './accounts/market.js';
import { RiskProfile, RiskProfileOrderRef } from './accounts/riskProfile.js';
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

/** Fetch + decode the per-mint GlobalVault. */
export async function fetchVault(
  conn: Connection,
  mint: PublicKey,
): Promise<GlobalVault | null> {
  const [vault] = globalVaultPda(mint);
  const acc = await conn.getAccountInfo(vault);
  if (!acc) return null;
  return decodeGlobalVault(acc.data);
}

/** Find a `RiskProfile` by `(vault, profile_id)`. */
export async function fetchRiskProfile(
  conn: Connection,
  mint: PublicKey,
  profileId: number,
): Promise<RiskProfile | null> {
  const v = await fetchVault(conn, mint);
  if (!v) return null;
  const entry = v.riskProfiles.find((p) => p.profile.profileId === profileId);
  return entry?.profile ?? null;
}

/** List every `RiskProfile` whose `curator` matches the given pubkey. */
export async function listRiskProfilesForCurator(
  conn: Connection,
  mint: PublicKey,
  curator: PublicKey,
): Promise<RiskProfile[]> {
  const v = await fetchVault(conn, mint);
  if (!v) return [];
  return v.riskProfiles
    .map((p) => p.profile)
    .filter((p) => p.curator.equals(curator) || p.pendingCurator.equals(curator));
}

/** List every market where `profileId` has a live ask. */
export async function listMarketsForRiskProfile(
  conn: Connection,
  mint: PublicKey,
  profileId: number,
): Promise<RiskProfileOrderRef[]> {
  const v = await fetchVault(conn, mint);
  if (!v) return [];
  return v.marketOrders
    .map((m) => m.order)
    .filter((o) => o.profileId === profileId);
}
