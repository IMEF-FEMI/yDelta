/**
 * setup-curator-profiles.ts — create N risk profiles in a vault, each
 * bound to a curator from `.local/curators.json`.
 *
 * Reads:
 *   .local/setup-curator-profiles-input.json {
 *     mint: string,
 *     curatorLabel?: string,                 // default for all profiles
 *     profiles: [{
 *       curatorLabel?: string,               // overrides default per profile
 *       maxLtvBps: number,
 *       maxTermSeconds: number
 *     }]
 *   }
 *   .local/vaults.json
 *   .local/curators.json
 *
 * Writes:
 *   .local/curator-setup.json {
 *     mint,
 *     profiles: [{
 *       profileId, curator, curatorLabel, maxLtvBps, maxTermSeconds, signature?
 *     }],
 *     createdAtUnix
 *   }
 *   .local/risk-profiles.json   (keyed by mint)
 *
 * ## profile_id is program-assigned
 *
 * The on-chain `create_risk_profile` ix assigns `profile_id` from the
 * vault's monotonic `next_profile_id` counter — callers cannot request
 * a specific id. Input profiles therefore no longer specify `profileId`.
 * For each create the script:
 *   1. Reads the vault's live `next_profile_id` (pre-create snapshot).
 *   2. Sends `create_risk_profile`.
 *   3. Records the snapshot as the assigned id.
 *
 * ## Idempotency
 *
 * Re-runs match input rows against already-recorded rows by the
 * (curatorLabel, maxLtvBps, maxTermSeconds) tuple. A row whose triple
 * matches an existing record is skipped — it's already on-chain. Rows
 * with new triples are created. This is intentionally **append-only**:
 * the input file is a "we want at least these profiles" declaration,
 * not a full reconciliation target.
 */
import { PublicKey } from '@solana/web3.js';

import { createRiskProfileInstruction } from '../src/instructions/index.js';
import { decodeGlobalVaultHeader } from '../src/accounts/vault.js';
import { globalVaultPda } from '../src/pdas.js';
import {
  appendTxLog,
  loadConnection,
  loadSigner,
  log,
  readJson,
  readJsonOptional,
  sendIxs,
  writeJson,
} from './_runner.js';
import type { CuratorDump, ProfileDump, VaultDump } from './_types.js';

interface InputProfile {
  curatorLabel?: string;
  maxLtvBps: number;
  maxTermSeconds: number;
}

interface Input {
  mint: string;
  curatorLabel?: string;
  profiles: InputProfile[];
}

type CuratorSetupProfile = ProfileDump;

interface CuratorSetup {
  mint: string;
  profiles: CuratorSetupProfile[];
  createdAtUnix: number;
}

function resolveCuratorForProfile(
  curators: CuratorDump[],
  topLevelCuratorLabel: string | undefined,
  profile: InputProfile,
  rowIndex: number,
): CuratorDump {
  const label = profile.curatorLabel ?? topLevelCuratorLabel;
  if (!label) {
    throw new Error(
      `profiles[${rowIndex}]: missing curatorLabel. ` +
        `Set it per profile or provide a top-level curatorLabel for single-curator setups.`,
    );
  }
  const curator = curators.find((c) => c.label === label);
  if (!curator) {
    throw new Error(
      `curators.json: no curator with label ${label} for profiles[${rowIndex}]`,
    );
  }
  return curator;
}

function recordedMatches(
  recorded: CuratorSetupProfile[],
  wanted: Omit<CuratorSetupProfile, 'profileId' | 'signature'>,
): CuratorSetupProfile | undefined {
  return recorded.find(
    (r) =>
      r.curatorLabel === wanted.curatorLabel &&
      r.curator === wanted.curator &&
      r.maxLtvBps === wanted.maxLtvBps &&
      r.maxTermSeconds === wanted.maxTermSeconds,
  );
}

async function readNextProfileId(
  conn: ReturnType<typeof loadConnection>,
  vaultPda: PublicKey,
): Promise<number> {
  const info = await conn.getAccountInfo(vaultPda, 'confirmed');
  if (!info) {
    throw new Error(
      `vault account ${vaultPda.toBase58()} not found — has create-vault run on this mint?`,
    );
  }
  const header = decodeGlobalVaultHeader(info.data);
  return header.nextProfileId;
}

async function main(): Promise<void> {
  const input = readJson<Input>('setup-curator-profiles-input.json');
  const vaults = readJson<Record<string, VaultDump>>('vaults.json');
  const curators = readJson<CuratorDump[]>('curators.json');
  if (!vaults[input.mint]) {
    throw new Error(`vaults.json: no vault for mint ${input.mint} (run create-vault first)`);
  }
  if (curators.length === 0) {
    throw new Error('curators.json is empty (run create-curators first)');
  }

  const existing = readJsonOptional<CuratorSetup>('curator-setup.json');
  const setup: CuratorSetup = existing ?? {
    mint: input.mint,
    profiles: [],
    createdAtUnix: Math.floor(Date.now() / 1000),
  };
  if (setup.mint !== input.mint) {
    throw new Error(
      `curator-setup.json already targets mint ${setup.mint}; requested ${input.mint}`,
    );
  }

  const conn = loadConnection();
  const signer = loadSigner();
  const mint = new PublicKey(input.mint);
  const vaultPda = globalVaultPda(mint)[0];

  for (let i = 0; i < input.profiles.length; i++) {
    const p = input.profiles[i]!;
    const curator = resolveCuratorForProfile(curators, input.curatorLabel, p, i);
    const wantedTriple = {
      curator: curator.pubkey,
      curatorLabel: curator.label,
      maxLtvBps: p.maxLtvBps,
      maxTermSeconds: p.maxTermSeconds,
    };

    const already = recordedMatches(setup.profiles, wantedTriple);
    if (already) {
      log(
        `[setup-curator-profiles] (curator=${curator.label}, ltv=${p.maxLtvBps}bps, ` +
          `term=${p.maxTermSeconds}s) already recorded as profileId=${already.profileId}; skipping`,
      );
      continue;
    }

    // Snapshot the vault's next_profile_id BEFORE submitting. The
    // program assigns this exact id to the new profile (the counter is
    // bumped inside the ix, but we capture it first so we know what
    // landed without re-reading or log-parsing).
    const assignedProfileId = await readNextProfileId(conn, vaultPda);

    log(
      `[setup-curator-profiles] mint=${input.mint} curator=${curator.pubkey} ` +
        `maxLtv=${p.maxLtvBps}bps maxTerm=${p.maxTermSeconds}s → profileId=${assignedProfileId}`,
    );
    const ix = createRiskProfileInstruction({
      payer: signer.publicKey,
      mint,
      curator: new PublicKey(curator.pubkey),
      maxLtvBps: p.maxLtvBps,
      maxTermSeconds: p.maxTermSeconds,
    });
    const sig = await sendIxs(conn, signer, [ix]);
    log(`[setup-curator-profiles]   signature = ${sig}`);

    const created: CuratorSetupProfile = {
      profileId: assignedProfileId,
      ...wantedTriple,
      signature: sig,
    };
    setup.profiles.push(created);
    appendTxLog({
      script: 'setup-curator-profiles',
      signatures: [sig],
      summary: {
        mint: input.mint,
        profileId: assignedProfileId,
        curator: curator.pubkey,
      },
    });
  }

  setup.profiles.sort((a, b) => a.profileId - b.profileId);
  writeJson('curator-setup.json', setup);

  const profilesByMint =
    readJsonOptional<Record<string, ProfileDump[]>>('risk-profiles.json') ?? {};
  profilesByMint[input.mint] = setup.profiles;
  writeJson('risk-profiles.json', profilesByMint);
  log(`[setup-curator-profiles] wrote .local/curator-setup.json and risk-profiles.json`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
