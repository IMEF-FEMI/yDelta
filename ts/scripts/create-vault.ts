/**
 * create-vault.ts — fires `CreateVault` (tag 8).
 *
 * Reads:
 *   .local/global-config.json
 *   .local/vault-input.json {
 *     mint: string,                    // debt mint
 *     bank: string,                    // marginfi `Bank` for this mint (= lending_pool)
 *     marginfiGroup: string,           // marginfi `MarginfiGroup`
 *     marginfiProgram?: string         // optional; defaults to the v0.1.8 program id
 *   }
 * Writes:
 *   .local/vaults.json {
 *     [mint]: { vaultPda, signerPda, integrationAccount, stagingPda, bank, group, signature? }
 *   }
 *
 * Every operator-supplied account is explicit in `vault-input.json` so the
 * file can be diffed against `solana account <pubkey>` before signing. We
 * still read the on-chain `Bank` and assert mint/group/etc match the input
 * (fail-loud if the operator pasted a wrong pubkey).
 */
import { PublicKey } from '@solana/web3.js';

import { decodeGlobalVaultHeader } from '../src/accounts/vault.js';
import { createVaultInstruction } from '../src/instructions/index.js';
import {
  globalVaultIntegrationAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
} from '../src/pdas.js';
import {
  appendTxLog,
  loadConnection,
  loadGlobalConfig,
  loadSigner,
  log,
  readJson,
  readJsonOptional,
  sendIxs,
  writeJson,
} from './_runner.js';
import { MARGINFI_PROGRAM_ID, readBankOnchain } from './_marginfi.js';

interface VaultInput {
  mint: string;
  bank: string;
  marginfiGroup: string;
  marginfiProgram?: string;
}

interface VaultDump {
  vaultPda: string;
  signerPda: string;
  integrationAccount: string;
  stagingPda: string;
  bank: string;
  group: string;
  signature?: string;
  alreadyExisted?: boolean;
}

async function main(): Promise<void> {
  loadGlobalConfig(); // existence check only
  const input = readJson<VaultInput>('vault-input.json');
  const mint = new PublicKey(input.mint);

  const conn = loadConnection();
  const signer = loadSigner();

  const vaults = readJsonOptional<Record<string, VaultDump>>('vaults.json') ?? {};
  if (vaults[input.mint]) {
    log(`[create-vault] vault for mint ${input.mint} already in vaults.json; skipping`);
    return;
  }

  const bankPubkey = new PublicKey(input.bank);
  const groupPubkey = new PublicKey(input.marginfiGroup);
  const marginfiProgram = new PublicKey(input.marginfiProgram ?? MARGINFI_PROGRAM_ID);
  if (!marginfiProgram.equals(MARGINFI_PROGRAM_ID)) {
    throw new Error(
      `vault-input.json marginfiProgram ${marginfiProgram.toBase58()} ≠ pinned v0.1.8 ` +
      `program id ${MARGINFI_PROGRAM_ID.toBase58()}`,
    );
  }

  // Verify the bank on-chain matches the input (mint, group, owner).
  const bank = await readBankOnchain(conn, bankPubkey);
  if (!bank.mint.equals(mint)) {
    throw new Error(
      `bank ${bankPubkey.toBase58()} has mint ${bank.mint.toBase58()} ≠ input.mint ${mint.toBase58()}`,
    );
  }
  if (!bank.group.equals(groupPubkey)) {
    throw new Error(
      `bank ${bankPubkey.toBase58()} group ${bank.group.toBase58()} ≠ input.marginfiGroup ${groupPubkey.toBase58()}`,
    );
  }

  const [vault] = globalVaultPda(mint);
  const onchainVault = await conn.getAccountInfo(vault);
  if (onchainVault) {
    const header = decodeGlobalVaultHeader(onchainVault.data);
    if (!header.mint.equals(mint)) {
      throw new Error(
        `vault ${vault.toBase58()} mint ${header.mint.toBase58()} does not match requested mint ${mint.toBase58()}`,
      );
    }
    log(`[create-vault] vault ${vault.toBase58()} already exists on-chain; recording without sending`);
    vaults[input.mint] = buildDump(
      vault,
      header.lendingPool,
      header.integrationPool,
      undefined,
      true,
    );
    writeJson('vaults.json', vaults);
    return;
  }

  log(`[create-vault] mint = ${mint.toBase58()}`);
  log(`[create-vault] bank = ${bankPubkey.toBase58()}`);
  log(`[create-vault] group = ${groupPubkey.toBase58()}`);
  log(`[create-vault] vault pda = ${vault.toBase58()}`);

  const ix = createVaultInstruction({
    payer: signer.publicKey,
    mint,
    marginfiGroup: groupPubkey,
    lendingPool: bankPubkey,
    marginfiProgram,
  });
  const sig = await sendIxs(conn, signer, [ix]);
  log(`[create-vault] signature = ${sig}`);
  vaults[input.mint] = buildDump(vault, bankPubkey, groupPubkey, sig, false);
  writeJson('vaults.json', vaults);
  appendTxLog({
    script: 'create-vault',
    signatures: [sig],
    summary: {
      mint: input.mint,
      vault: vault.toBase58(),
      bank: bankPubkey.toBase58(),
      group: groupPubkey.toBase58(),
    },
  });
}

function buildDump(
  vault: PublicKey,
  bank: PublicKey,
  group: PublicKey,
  signature: string | undefined,
  alreadyExisted: boolean,
): VaultDump {
  return {
    vaultPda: vault.toBase58(),
    signerPda: globalVaultSignerPda(vault)[0].toBase58(),
    integrationAccount: globalVaultIntegrationAccountPda(vault)[0].toBase58(),
    stagingPda: globalVaultStagingPda(vault)[0].toBase58(),
    bank: bank.toBase58(),
    group: group.toBase58(),
    ...(signature ? { signature } : {}),
    ...(alreadyExisted ? { alreadyExisted: true } : {}),
  };
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
