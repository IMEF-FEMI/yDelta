/**
 * `deploy.ts` — one-shot `CreateGlobalConfig` instruction. The signer
 * becomes the initial `protocol_admin`. Requires `--program-data <pubkey>`
 * (the BPF Upgradeable Loader's ProgramData PDA for the yDelta program).
 *
 * Usage:
 *   tsx ts/scripts/deploy.ts --program-data <PROGRAM_DATA_PUBKEY> [--dry-run]
 */
import { createGlobalConfigInstruction } from '../src/instructions/index.js';
import { loadConnection, loadSigner, pubkeyFlag, runScript } from './_runner.js';

async function main(): Promise<void> {
  const conn = loadConnection();
  const signer = loadSigner();
  const programData = pubkeyFlag('program-data');

  const ix = createGlobalConfigInstruction({
    payer: signer.publicKey,
    programData,
  });
  await runScript({ conn, signer, ixs: [ix] });
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
