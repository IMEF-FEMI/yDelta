/**
 * Shared `.local/` JSON shapes consumed by multiple scripts. Each script
 * file owns its own input shape, but the artifact-shape JSONs (markets,
 * vaults, sub-vaults, curators) are shared and live here so the
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

export interface SubVaultDump {
  subVaultId: number;
  /** `Pool` (curator-managed) or `Private` (self-managed) sub-vault. */
  kind: 'Pool' | 'Private';
  curator: string;
  curatorLabel: string;
  spreadBps: number;
  maxLtvBps: number;
  liquidationLtvBps: number;
  maxTermSeconds: number;
  curatorFeeBps: number;
  signature?: string;
}

export interface CuratorDump {
  label: string;
  pubkey: string;
  secretKeyBase58: string;
}

export function resolveCuratorForSubVault(
  subVaults: Record<string, SubVaultDump[]>,
  curators: CuratorDump[],
  mint: string,
  subVaultId: number,
): CuratorDump {
  const arr = subVaults[mint];
  if (!arr) throw new Error(`sub-vaults.json: no entries for mint ${mint}`);
  const sv = arr.find((x) => x.subVaultId === subVaultId);
  if (!sv) throw new Error(`sub-vaults.json[${mint}]: no subVaultId=${subVaultId}`);
  const c = curators.find((x) => x.pubkey === sv.curator);
  if (!c) {
    throw new Error(
      `curators.json: no curator with pubkey ${sv.curator} (subVaultId=${subVaultId}). ` +
      `Did curators.json get regenerated after sub-vaults.json was written?`,
    );
  }
  return c;
}
