/**
 * `create-vault.ts` — one-shot `CreateVault`. First caller per mint becomes
 * the `global_vault_admin`.
 *
 * Flags:
 *   --mint <PUBKEY>
 *   --marginfi-group <PUBKEY>
 *   --lending-pool <PUBKEY>           (the marginfi Bank this vault pins to)
 *   --marginfi-program <PUBKEY>
 */
import { createVaultInstruction } from '../src/instructions/index.js';
import { loadConnection, loadSigner, pubkeyFlag, runScript } from './_runner.js';

async function main(): Promise<void> {
  const conn = loadConnection();
  const signer = loadSigner();
  const ix = createVaultInstruction({
    payer: signer.publicKey,
    mint: pubkeyFlag('mint'),
    marginfiGroup: pubkeyFlag('marginfi-group'),
    lendingPool: pubkeyFlag('lending-pool'),
    marginfiProgram: pubkeyFlag('marginfi-program'),
  });
  await runScript({ conn, signer, ixs: [ix] });
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
