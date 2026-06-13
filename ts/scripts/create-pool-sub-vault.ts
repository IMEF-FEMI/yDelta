/**
 * create-pool-sub-vault.ts — `CreatePoolSubVault` (tag 9). One-shot
 * create for a single curator-managed (Pool) sub-vault.
 *
 * Reads:
 *   .local/create-pool-sub-vault-input.json {
 *     mint: string,
 *     curatorLabel: string,        // resolved against .local/curators.json
 *     spreadBps: number,
 *     maxLtvBps: number,
 *     liquidationLtvBps: number,
 *     maxTermSeconds: number,
 *     curatorFeeBps: number
 *   }
 *   .local/vaults.json
 *   .local/curators.json
 *
 * Writes (or extends):
 *   .local/risk-profiles.json   (keyed by mint; appends the new sub-vault)
 *
 * ## sub_vault_id is program-assigned
 *
 * The on-chain `create_pool_sub_vault` ix assigns `sub_vault_id` from the
 * vault's monotonic `next_sub_vault_id` counter. This script snapshots
 * the counter BEFORE sending the ix so the assigned id is captured
 * deterministically (without needing to parse logs).
 */
import { PublicKey } from '@solana/web3.js';

import { decodeGlobalVaultHeader } from '../src/accounts/vault.js';
import { createPoolSubVaultInstruction } from '../src/instructions/index.js';
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

interface Input {
  mint: string;
  curatorLabel: string;
  spreadBps: number;
  maxLtvBps: number;
  liquidationLtvBps: number;
  maxTermSeconds: number;
  curatorFeeBps: number;
}

async function main(): Promise<void> {
  const input = readJson<Input>('create-pool-sub-vault-input.json');
  const vaults = readJson<Record<string, VaultDump>>('vaults.json');
  const curators = readJson<CuratorDump[]>('curators.json');
  const vault = vaults[input.mint];
  if (!vault) {
    throw new Error(`vaults.json: no vault for mint ${input.mint} (run create-vault first)`);
  }
  const curator = curators.find((c) => c.label === input.curatorLabel);
  if (!curator) {
    throw new Error(
      `curators.json: no curator with label "${input.curatorLabel}" — run create-curators first`,
    );
  }

  const conn = loadConnection();
  const signer = loadSigner();
  // The vault PDA is bank-keyed (`[b"vault", bank]`); resolve the bank from
  // the recorded vault dump rather than the mint.
  const bank = new PublicKey(vault.bank);
  const vaultPda = globalVaultPda(bank)[0];

  // Pre-read the vault's monotonic counter so we know which id the
  // program is about to assign. The ix bumps it inside the program;
  // capturing it now lets us record the assignment without re-reading
  // or parsing the emitted log.
  const vaultInfo = await conn.getAccountInfo(vaultPda, 'confirmed');
  if (!vaultInfo) {
    throw new Error(
      `vault account ${vaultPda.toBase58()} not found — has create-vault run on this mint?`,
    );
  }
  const header = decodeGlobalVaultHeader(vaultInfo.data);
  const assignedSubVaultId = header.nextSubVaultId;

  log(
    `[create-pool-sub-vault] mint=${input.mint} curator=${curator.pubkey} ` +
      `spread=${input.spreadBps}bps maxLtv=${input.maxLtvBps}bps liqLtv=${input.liquidationLtvBps}bps ` +
      `maxTerm=${input.maxTermSeconds}s curatorFee=${input.curatorFeeBps}bps → subVaultId=${assignedSubVaultId}`,
  );

  const ix = createPoolSubVaultInstruction({
    payer: signer.publicKey,
    bank,
    curator: new PublicKey(curator.pubkey),
    spreadBps: input.spreadBps,
    maxLtvBps: input.maxLtvBps,
    liquidationLtvBps: input.liquidationLtvBps,
    maxTermSeconds: input.maxTermSeconds,
    curatorFeeBps: input.curatorFeeBps,
  });
  const sig = await sendIxs(conn, signer, [ix]);
  log(`[create-pool-sub-vault] signature = ${sig}`);

  // Persist to risk-profiles.json (the canonical artifact downstream
  // scripts read). Appends; does not deduplicate by content — the
  // operator is expected to invoke this script for a NEW sub-vault.
  const subVaultsByMint =
    readJsonOptional<Record<string, SubVaultDump[]>>('risk-profiles.json') ?? {};
  const existing = subVaultsByMint[input.mint] ?? [];
  const created: SubVaultDump = {
    subVaultId: assignedSubVaultId,
    kind: 'Pool',
    curator: curator.pubkey,
    curatorLabel: curator.label,
    spreadBps: input.spreadBps,
    maxLtvBps: input.maxLtvBps,
    liquidationLtvBps: input.liquidationLtvBps,
    maxTermSeconds: input.maxTermSeconds,
    curatorFeeBps: input.curatorFeeBps,
    signature: sig,
  };
  subVaultsByMint[input.mint] = [...existing, created].sort(
    (a, b) => a.subVaultId - b.subVaultId,
  );
  writeJson('risk-profiles.json', subVaultsByMint);

  appendTxLog({
    script: 'create-pool-sub-vault',
    signatures: [sig],
    summary: {
      mint: input.mint,
      subVaultId: assignedSubVaultId,
      curator: curator.pubkey,
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
