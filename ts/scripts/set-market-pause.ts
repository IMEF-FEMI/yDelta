/**
 * `set-market-pause.ts` — admin pause/unpause toggle.
 *
 * Flags:
 *   --market <PUBKEY>
 *   --paused                  (set to pause; omit for unpause)
 */
import { setMarketPauseInstruction } from '../src/instructions/index.js';
import { hasFlag, loadConnection, loadSigner, pubkeyFlag, runScript } from './_runner.js';

async function main(): Promise<void> {
  const conn = loadConnection();
  const signer = loadSigner();
  const ix = setMarketPauseInstruction({
    admin: signer.publicKey,
    market: pubkeyFlag('market'),
    paused: hasFlag('paused'),
  });
  await runScript({ conn, signer, ixs: [ix] });
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
