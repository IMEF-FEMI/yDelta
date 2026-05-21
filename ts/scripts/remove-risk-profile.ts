/**
 * remove-risk-profile.ts — `RemoveRiskProfile` (tag 37). Tear down an
 * empty `RiskProfile` from the vault.
 *
 * Reads:
 *   .local/remove-risk-profile-input.json {
 *     mint: string,
 *     profileId: number
 *   }
 *
 * Updates (after success):
 *   .local/risk-profiles.json   (removes the entry under [mint])
 *
 * ## Safety
 *
 * The on-chain processor rejects with `VaultProfileNotEmpty` (code 34)
 * unless every balance field on the profile is zero. This script does
 * NOT pre-check those fields — it relies on the on-chain gate. Operators
 * can therefore invoke this freely; a still-active profile fails the
 * tx cleanly without mutating any state.
 *
 * The vault's `next_profile_id` counter is NOT decremented by the
 * program — the removed id is retired forever, so historical loan /
 * log references stay unambiguous.
 */
import { PublicKey } from '@solana/web3.js';

import { removeRiskProfileInstruction } from '../src/instructions/index.js';
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
import type { ProfileDump } from './_types.js';

interface Input {
  mint: string;
  profileId: number;
}

async function main(): Promise<void> {
  const input = readJson<Input>('remove-risk-profile-input.json');

  const conn = loadConnection();
  const signer = loadSigner();
  const mint = new PublicKey(input.mint);

  log(
    `[remove-risk-profile] mint=${input.mint} profileId=${input.profileId}`,
  );
  const ix = removeRiskProfileInstruction({
    payer: signer.publicKey,
    mint,
    profileId: input.profileId,
  });
  const sig = await sendIxs(conn, signer, [ix]);
  log(`[remove-risk-profile] signature = ${sig}`);

  // Strip the entry from risk-profiles.json so downstream scripts can't
  // resolve it any more. The chain is the source of truth; this is just
  // local artifact hygiene.
  const profilesByMint =
    readJsonOptional<Record<string, ProfileDump[]>>('risk-profiles.json') ?? {};
  const existing = profilesByMint[input.mint] ?? [];
  const next = existing.filter((p) => p.profileId !== input.profileId);
  if (next.length !== existing.length) {
    profilesByMint[input.mint] = next;
    writeJson('risk-profiles.json', profilesByMint);
    log(
      `[remove-risk-profile] removed profileId=${input.profileId} from risk-profiles.json[${input.mint}]`,
    );
  } else {
    log(
      `[remove-risk-profile] note: profileId=${input.profileId} was not present in risk-profiles.json — tx succeeded on-chain, local artifact unchanged`,
    );
  }

  appendTxLog({
    script: 'remove-risk-profile',
    signatures: [sig],
    summary: {
      mint: input.mint,
      profileId: input.profileId,
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
