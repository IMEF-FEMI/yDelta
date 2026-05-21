/**
 * create-risk-profile.ts — `CreateRiskProfile` (tag 9). One-shot create
 * for a single risk profile.
 *
 * Reads:
 *   .local/create-risk-profile-input.json {
 *     mint: string,
 *     curatorLabel: string,        // resolved against .local/curators.json
 *     maxLtvBps: number,
 *     maxTermSeconds: number
 *   }
 *
 * Writes (or extends):
 *   .local/risk-profiles.json   (keyed by mint; appends the new profile)
 *
 * ## profile_id is program-assigned
 *
 * The on-chain `create_risk_profile` ix assigns `profile_id` from the
 * vault's monotonic `next_profile_id` counter. This script snapshots
 * the counter BEFORE sending the ix so the assigned id is captured
 * deterministically (without needing to parse logs).
 */
import { PublicKey } from '@solana/web3.js';

import { decodeGlobalVaultHeader } from '../src/accounts/vault.js';
import { createRiskProfileInstruction } from '../src/instructions/index.js';
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
import type { CuratorDump, ProfileDump } from './_types.js';

interface Input {
  mint: string;
  curatorLabel: string;
  maxLtvBps: number;
  maxTermSeconds: number;
}

async function main(): Promise<void> {
  const input = readJson<Input>('create-risk-profile-input.json');
  const curators = readJson<CuratorDump[]>('curators.json');
  const curator = curators.find((c) => c.label === input.curatorLabel);
  if (!curator) {
    throw new Error(
      `curators.json: no curator with label "${input.curatorLabel}" — run create-curators first`,
    );
  }

  const conn = loadConnection();
  const signer = loadSigner();
  const mint = new PublicKey(input.mint);
  const vaultPda = globalVaultPda(mint)[0];

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
  const assignedProfileId = header.nextProfileId;

  log(
    `[create-risk-profile] mint=${input.mint} curator=${curator.pubkey} ` +
      `maxLtv=${input.maxLtvBps}bps maxTerm=${input.maxTermSeconds}s → profileId=${assignedProfileId}`,
  );

  const ix = createRiskProfileInstruction({
    payer: signer.publicKey,
    mint,
    curator: new PublicKey(curator.pubkey),
    maxLtvBps: input.maxLtvBps,
    maxTermSeconds: input.maxTermSeconds,
  });
  const sig = await sendIxs(conn, signer, [ix]);
  log(`[create-risk-profile] signature = ${sig}`);

  // Persist to risk-profiles.json (the canonical artifact downstream
  // scripts read). Appends; does not deduplicate by content — the
  // operator is expected to invoke this script for a NEW profile.
  const profilesByMint =
    readJsonOptional<Record<string, ProfileDump[]>>('risk-profiles.json') ?? {};
  const existing = profilesByMint[input.mint] ?? [];
  const created: ProfileDump = {
    profileId: assignedProfileId,
    curator: curator.pubkey,
    curatorLabel: curator.label,
    maxLtvBps: input.maxLtvBps,
    maxTermSeconds: input.maxTermSeconds,
    signature: sig,
  };
  profilesByMint[input.mint] = [...existing, created].sort(
    (a, b) => a.profileId - b.profileId,
  );
  writeJson('risk-profiles.json', profilesByMint);

  appendTxLog({
    script: 'create-risk-profile',
    signatures: [sig],
    summary: {
      mint: input.mint,
      profileId: assignedProfileId,
      curator: curator.pubkey,
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
