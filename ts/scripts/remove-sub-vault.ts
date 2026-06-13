/**
 * remove-sub-vault.ts — `RemoveSubVault` (tag 37). Tear down an
 * empty `SubVault` from the vault.
 *
 * Reads:
 *   .local/remove-sub-vault-input.json {
 *     mint: string,
 *     subVaultId: number
 *   }
 *   .local/vaults.json
 *
 * Updates (after success):
 *   .local/risk-profiles.json   (removes the entry under [mint])
 *
 * ## Safety
 *
 * The on-chain processor rejects with `SubVaultNotEmpty` (code 34)
 * unless every balance field on the sub-vault is zero. This script does
 * NOT pre-check those fields — it relies on the on-chain gate. Operators
 * can therefore invoke this freely; a still-active sub-vault fails the
 * tx cleanly without mutating any state.
 *
 * The vault's `next_sub_vault_id` counter is NOT decremented by the
 * program — the removed id is retired forever, so historical loan /
 * log references stay unambiguous.
 */
import { PublicKey } from '@solana/web3.js';

import { removeSubVaultInstruction } from '../src/instructions/index.js';
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
import type { SubVaultDump, VaultDump } from './_types.js';

interface Input {
  mint: string;
  subVaultId: number;
}

async function main(): Promise<void> {
  const input = readJson<Input>('remove-sub-vault-input.json');
  const vaults = readJson<Record<string, VaultDump>>('vaults.json');
  const vault = vaults[input.mint];
  if (!vault) {
    throw new Error(`vaults.json: no vault for mint ${input.mint} (run create-vault first)`);
  }

  const conn = loadConnection();
  const signer = loadSigner();
  // The vault PDA is bank-keyed (`[b"vault", bank]`); resolve the bank from
  // the recorded vault dump rather than the mint.
  const bank = new PublicKey(vault.bank);

  log(
    `[remove-sub-vault] mint=${input.mint} subVaultId=${input.subVaultId}`,
  );
  const ix = removeSubVaultInstruction({
    payer: signer.publicKey,
    bank,
    subVaultId: input.subVaultId,
  });
  const sig = await sendIxs(conn, signer, [ix]);
  log(`[remove-sub-vault] signature = ${sig}`);

  // Strip the entry from risk-profiles.json so downstream scripts can't
  // resolve it any more. The chain is the source of truth; this is just
  // local artifact hygiene.
  const subVaultsByMint =
    readJsonOptional<Record<string, SubVaultDump[]>>('risk-profiles.json') ?? {};
  const existing = subVaultsByMint[input.mint] ?? [];
  const next = existing.filter((p) => p.subVaultId !== input.subVaultId);
  if (next.length !== existing.length) {
    subVaultsByMint[input.mint] = next;
    writeJson('risk-profiles.json', subVaultsByMint);
    log(
      `[remove-sub-vault] removed subVaultId=${input.subVaultId} from risk-profiles.json[${input.mint}]`,
    );
  } else {
    log(
      `[remove-sub-vault] note: subVaultId=${input.subVaultId} was not present in risk-profiles.json — tx succeeded on-chain, local artifact unchanged`,
    );
  }

  appendTxLog({
    script: 'remove-sub-vault',
    signatures: [sig],
    summary: {
      mint: input.mint,
      subVaultId: input.subVaultId,
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
