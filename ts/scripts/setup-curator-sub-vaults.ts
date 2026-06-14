/**
 * setup-curator-sub-vaults.ts — create N pool sub-vaults in a vault, each
 * bound to a curator from `.local/curators.json`.
 *
 * Reads:
 *   .local/setup-curator-sub-vaults-input.json {
 *     mint: string,
 *     curatorLabel?: string,                 // default for all sub-vaults
 *     subVaults: [{
 *       curatorLabel?: string,               // overrides default per sub-vault
 *       spreadBps: number,
 *       maxLtvBps: number,
 *       liquidationLtvBps: number,
 *       maxTermSeconds: number,
 *       curatorFeeBps: number
 *     }]
 *   }
 *   .local/vaults.json
 *   .local/curators.json
 *
 * Writes:
 *   .local/curator-setup.json {
 *     mint,
 *     subVaults: [{
 *       subVaultId, kind, curator, curatorLabel, spreadBps, maxLtvBps,
 *       liquidationLtvBps, maxTermSeconds, curatorFeeBps, signature?
 *     }],
 *     createdAtUnix
 *   }
 *   .local/sub-vaults.json   (keyed by mint)
 *
 * ## sub_vault_id is program-assigned
 *
 * The on-chain `create_pool_sub_vault` ix assigns `sub_vault_id` from the
 * vault's monotonic `next_sub_vault_id` counter — callers cannot request
 * a specific id. Input sub-vaults therefore no longer specify an id.
 * For each create the script:
 *   1. Reads the vault's live `next_sub_vault_id` (pre-create snapshot).
 *   2. Sends `create_pool_sub_vault`.
 *   3. Records the snapshot as the assigned id.
 *
 * ## Idempotency
 *
 * Re-runs match input rows against already-recorded rows by the
 * (curatorLabel, spreadBps, maxLtvBps, liquidationLtvBps, maxTermSeconds,
 * curatorFeeBps) tuple. A row whose tuple matches an existing record is
 * skipped — it's already on-chain. Rows with new tuples are created. This
 * is intentionally **append-only**: the input file is a "we want at least
 * these sub-vaults" declaration, not a full reconciliation target.
 */
import { PublicKey } from '@solana/web3.js';

import { createPoolSubVaultInstruction } from '../src/instructions/index.js';
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
import type { CuratorDump, SubVaultDump, VaultDump } from './_types.js';

interface InputSubVault {
  curatorLabel?: string;
  spreadBps: number;
  maxLtvBps: number;
  liquidationLtvBps: number;
  maxTermSeconds: number;
  curatorFeeBps: number;
}

interface Input {
  mint: string;
  curatorLabel?: string;
  subVaults: InputSubVault[];
}

type CuratorSetupSubVault = SubVaultDump;

interface CuratorSetup {
  mint: string;
  subVaults: CuratorSetupSubVault[];
  createdAtUnix: number;
}

function resolveCuratorForSubVault(
  curators: CuratorDump[],
  topLevelCuratorLabel: string | undefined,
  subVault: InputSubVault,
  rowIndex: number,
): CuratorDump {
  const label = subVault.curatorLabel ?? topLevelCuratorLabel;
  if (!label) {
    throw new Error(
      `subVaults[${rowIndex}]: missing curatorLabel. ` +
        `Set it per sub-vault or provide a top-level curatorLabel for single-curator setups.`,
    );
  }
  const curator = curators.find((c) => c.label === label);
  if (!curator) {
    throw new Error(
      `curators.json: no curator with label ${label} for subVaults[${rowIndex}]`,
    );
  }
  return curator;
}

function recordedMatches(
  recorded: CuratorSetupSubVault[],
  wanted: Omit<CuratorSetupSubVault, 'subVaultId' | 'kind' | 'signature'>,
): CuratorSetupSubVault | undefined {
  return recorded.find(
    (r) =>
      r.curatorLabel === wanted.curatorLabel &&
      r.curator === wanted.curator &&
      r.spreadBps === wanted.spreadBps &&
      r.maxLtvBps === wanted.maxLtvBps &&
      r.liquidationLtvBps === wanted.liquidationLtvBps &&
      r.maxTermSeconds === wanted.maxTermSeconds &&
      r.curatorFeeBps === wanted.curatorFeeBps,
  );
}

async function readNextSubVaultId(
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
  return header.nextSubVaultId;
}

async function main(): Promise<void> {
  const input = readJson<Input>('setup-curator-sub-vaults-input.json');
  const vaults = readJson<Record<string, VaultDump>>('vaults.json');
  const curators = readJson<CuratorDump[]>('curators.json');
  const vault = vaults[input.mint];
  if (!vault) {
    throw new Error(`vaults.json: no vault for mint ${input.mint} (run create-vault first)`);
  }
  if (curators.length === 0) {
    throw new Error('curators.json is empty (run create-curators first)');
  }

  const existing = readJsonOptional<CuratorSetup>('curator-setup.json');
  const setup: CuratorSetup = existing ?? {
    mint: input.mint,
    subVaults: [],
    createdAtUnix: Math.floor(Date.now() / 1000),
  };
  if (setup.mint !== input.mint) {
    throw new Error(
      `curator-setup.json already targets mint ${setup.mint}; requested ${input.mint}`,
    );
  }

  const conn = loadConnection();
  const signer = loadSigner();
  // The vault PDA is bank-keyed (`[b"vault", bank]`); resolve the bank from
  // the recorded vault dump rather than the mint.
  const bank = new PublicKey(vault.bank);
  const vaultPda = globalVaultPda(bank)[0];

  for (let i = 0; i < input.subVaults.length; i++) {
    const p = input.subVaults[i]!;
    const curator = resolveCuratorForSubVault(curators, input.curatorLabel, p, i);
    const wantedTuple = {
      curator: curator.pubkey,
      curatorLabel: curator.label,
      spreadBps: p.spreadBps,
      maxLtvBps: p.maxLtvBps,
      liquidationLtvBps: p.liquidationLtvBps,
      maxTermSeconds: p.maxTermSeconds,
      curatorFeeBps: p.curatorFeeBps,
    };

    const already = recordedMatches(setup.subVaults, wantedTuple);
    if (already) {
      log(
        `[setup-curator-sub-vaults] (curator=${curator.label}, spread=${p.spreadBps}bps, ` +
          `ltv=${p.maxLtvBps}bps, liqLtv=${p.liquidationLtvBps}bps, term=${p.maxTermSeconds}s, ` +
          `fee=${p.curatorFeeBps}bps) already recorded as subVaultId=${already.subVaultId}; skipping`,
      );
      continue;
    }

    // Snapshot the vault's next_sub_vault_id BEFORE submitting. The
    // program assigns this exact id to the new sub-vault (the counter is
    // bumped inside the ix, but we capture it first so we know what
    // landed without re-reading or log-parsing).
    const assignedSubVaultId = await readNextSubVaultId(conn, vaultPda);

    log(
      `[setup-curator-sub-vaults] mint=${input.mint} curator=${curator.pubkey} ` +
        `spread=${p.spreadBps}bps maxLtv=${p.maxLtvBps}bps liqLtv=${p.liquidationLtvBps}bps ` +
        `maxTerm=${p.maxTermSeconds}s curatorFee=${p.curatorFeeBps}bps → subVaultId=${assignedSubVaultId}`,
    );
    const ix = createPoolSubVaultInstruction({
      payer: signer.publicKey,
      bank,
      curator: new PublicKey(curator.pubkey),
      spreadBps: p.spreadBps,
      maxLtvBps: p.maxLtvBps,
      liquidationLtvBps: p.liquidationLtvBps,
      maxTermSeconds: p.maxTermSeconds,
      curatorFeeBps: p.curatorFeeBps,
    });
    const sig = await sendIxs(conn, signer, [ix]);
    log(`[setup-curator-sub-vaults]   signature = ${sig}`);

    const created: CuratorSetupSubVault = {
      subVaultId: assignedSubVaultId,
      kind: 'Pool',
      ...wantedTuple,
      signature: sig,
    };
    setup.subVaults.push(created);
    appendTxLog({
      script: 'setup-curator-sub-vaults',
      signatures: [sig],
      summary: {
        mint: input.mint,
        subVaultId: assignedSubVaultId,
        curator: curator.pubkey,
      },
    });
  }

  setup.subVaults.sort((a, b) => a.subVaultId - b.subVaultId);
  writeJson('curator-setup.json', setup);

  const subVaultsByMint =
    readJsonOptional<Record<string, SubVaultDump[]>>('sub-vaults.json') ?? {};
  subVaultsByMint[input.mint] = setup.subVaults;
  writeJson('sub-vaults.json', subVaultsByMint);
  log(`[setup-curator-sub-vaults] wrote .local/curator-setup.json and sub-vaults.json`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
