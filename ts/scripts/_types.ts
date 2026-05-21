/**
 * Shared `.local/` JSON shapes consumed by multiple scripts. Each script
 * file owns its own input shape, but the artifact-shape JSONs (markets,
 * vaults, risk-profiles, curators) are shared and live here so the
 * scripts agree on field names.
 */

export interface MarketDump {
  label: string;
  market: string;
  marketSecretKeyBase58: string;
  debtMint: string;
  collateralMint: string;
  debtBank: string;
  debtLiquidityVault: string;
  debtBankLiquidityVaultAuthority: string;
  debtOracles: string[];
  debtPythFeedIdHex?: string;
  debtPythShardId?: number;
  collateralBank: string;
  collateralLiquidityVault: string;
  collateralBankLiquidityVaultAuthority: string;
  collateralOracles: string[];
  collateralPythFeedIdHex?: string;
  collateralPythShardId?: number;
  marginfiGroup: string;
  signature?: string;
  /** Address Lookup Table covering this market's static health-check accounts. */
  lookupTable?: string;
}

export interface VaultDump {
  vaultPda: string;
  signerPda: string;
  integrationAccount: string;
  stagingPda: string;
  bank: string;
  group: string;
  signature?: string;
  alreadyExisted?: boolean;
}

export interface ProfileDump {
  profileId: number;
  curator: string;
  curatorLabel: string;
  maxLtvBps: number;
  maxTermSeconds: number;
  signature?: string;
}

export interface CuratorDump {
  label: string;
  pubkey: string;
  secretKeyBase58: string;
}

export function resolveCuratorForProfile(
  profiles: Record<string, ProfileDump[]>,
  curators: CuratorDump[],
  mint: string,
  profileId: number,
): CuratorDump {
  const arr = profiles[mint];
  if (!arr) throw new Error(`risk-profiles.json: no entries for mint ${mint}`);
  const p = arr.find((x) => x.profileId === profileId);
  if (!p) throw new Error(`risk-profiles.json[${mint}]: no profileId=${profileId}`);
  const c = curators.find((x) => x.pubkey === p.curator);
  if (!c) {
    throw new Error(
      `curators.json: no curator with pubkey ${p.curator} (profileId=${profileId}). ` +
      `Did curators.json get regenerated after risk-profiles.json was written?`,
    );
  }
  return c;
}
