/**
 * create-private-sub-vault.ts — `CreatePrivateSubVault` (tag 41).
 * Permissionless. The `payer` (signer) becomes the curator / sole
 * depositor and the curator fee is forced to 0.
 *
 * Reads:
 *   .local/create-private-sub-vault-input.json {
 *     mint: string,
 *     curatorLabel?: string,       // .local/curators.json; payer/curator.
 *                                  //   Omit to use the deployer signer.
 *     spreadBps: number,
 *     maxLtvBps: number,
 *     liquidationLtvBps: number,
 *     maxTermSeconds: number
 *   }
 *   .local/vaults.json
 *   .local/curators.json   (only when curatorLabel is set)
 *
 * Writes (or extends):
 *   .local/sub-vaults.json   (keyed by mint; appends the new sub-vault)
 *
 * ## sub_vault_id is program-assigned
 *
 * The on-chain `create_private_sub_vault` ix assigns `sub_vault_id` from
 * the vault's monotonic `next_sub_vault_id` counter. This script
 * snapshots the counter BEFORE sending the ix so the assigned id is
 * captured deterministically (without needing to parse logs).
 */
import { PublicKey } from '@solana/web3.js';

import { decodeGlobalVaultHeader } from '../src/accounts/vault.js';
import { createPrivateSubVaultInstruction } from '../src/instructions/index.js';
import { globalVaultPda } from '../src/pdas.js';
import {
  appendTxLog,
  loadConnection,
  loadSigner,
  log,
  readJson,
  readJsonOptional,
  readKeypairFromBase58,
  sendIxs,
  writeJson,
} from './_runner.js';
import type { CuratorDump, SubVaultDump, VaultDump } from './_types.js';

interface Input {
  mint: string;
  curatorLabel?: string;
  spreadBps: number;
  maxLtvBps: number;
  liquidationLtvBps: number;
  maxTermSeconds: number;
}

async function main(): Promise<void> {
  const input = readJson<Input>('create-private-sub-vault-input.json');
  const vaults = readJson<Record<string, VaultDump>>('vaults.json');
  const vault = vaults[input.mint];
  if (!vault) {
    throw new Error(`vaults.json: no vault for mint ${input.mint} (run create-vault first)`);
  }

  const conn = loadConnection();
  // The payer becomes the curator / sole depositor. Use the named curator
  // keypair when given, else the deployer signer.
  const signer = loadSigner();
  let curatorPubkey = signer.publicKey;
  let curatorLabel = 'deployer';
  let extraSigners: ReturnType<typeof readKeypairFromBase58>[] = [];
  if (input.curatorLabel) {
    const curators = readJson<CuratorDump[]>('curators.json');
    const entry = curators.find((c) => c.label === input.curatorLabel);
    if (!entry) {
      throw new Error(
        `curators.json: no curator with label "${input.curatorLabel}" — run create-curators first`,
      );
    }
    const curatorKp = readKeypairFromBase58(entry.secretKeyBase58);
    curatorPubkey = curatorKp.publicKey;
    curatorLabel = entry.label;
    extraSigners = [curatorKp];
  }

  // The vault PDA is bank-keyed (`[b"vault", bank]`); resolve the bank from
  // the recorded vault dump rather than the mint.
  const bank = new PublicKey(vault.bank);
  const vaultPda = globalVaultPda(bank)[0];

  // Pre-read the vault's monotonic counter so we know which id the
  // program is about to assign (the ix bumps it inside the program).
  const vaultInfo = await conn.getAccountInfo(vaultPda, 'confirmed');
  if (!vaultInfo) {
    throw new Error(
      `vault account ${vaultPda.toBase58()} not found — has create-vault run on this mint?`,
    );
  }
  const header = decodeGlobalVaultHeader(vaultInfo.data);
  const assignedSubVaultId = header.nextSubVaultId;

  log(
    `[create-private-sub-vault] mint=${input.mint} curator=${curatorPubkey.toBase58()} ` +
      `spread=${input.spreadBps}bps maxLtv=${input.maxLtvBps}bps liqLtv=${input.liquidationLtvBps}bps ` +
      `maxTerm=${input.maxTermSeconds}s → subVaultId=${assignedSubVaultId}`,
  );

  const ix = createPrivateSubVaultInstruction({
    payer: curatorPubkey,
    bank,
    spreadBps: input.spreadBps,
    maxLtvBps: input.maxLtvBps,
    liquidationLtvBps: input.liquidationLtvBps,
    maxTermSeconds: input.maxTermSeconds,
  });
  // When the curator differs from the deployer the curator must sign (it's
  // the payer); the deployer still relays + funds the tx fee.
  const sig = await sendIxs(conn, signer, [ix], extraSigners);
  log(`[create-private-sub-vault] signature = ${sig}`);

  const subVaultsByMint =
    readJsonOptional<Record<string, SubVaultDump[]>>('sub-vaults.json') ?? {};
  const existing = subVaultsByMint[input.mint] ?? [];
  const created: SubVaultDump = {
    subVaultId: assignedSubVaultId,
    kind: 'Private',
    curator: curatorPubkey.toBase58(),
    curatorLabel,
    spreadBps: input.spreadBps,
    maxLtvBps: input.maxLtvBps,
    liquidationLtvBps: input.liquidationLtvBps,
    maxTermSeconds: input.maxTermSeconds,
    // Private sub-vaults force the curator fee to 0 on-chain.
    curatorFeeBps: 0,
    signature: sig,
  };
  subVaultsByMint[input.mint] = [...existing, created].sort(
    (a, b) => a.subVaultId - b.subVaultId,
  );
  writeJson('sub-vaults.json', subVaultsByMint);

  appendTxLog({
    script: 'create-private-sub-vault',
    signatures: [sig],
    summary: {
      mint: input.mint,
      subVaultId: assignedSubVaultId,
      curator: curatorPubkey.toBase58(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
