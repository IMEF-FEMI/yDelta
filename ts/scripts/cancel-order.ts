/**
 * `cancel-order.ts` — curator cancels their vault ask. Idempotent on missing.
 *
 * Flags:
 *   --mint <PUBKEY>
 *   --market <PUBKEY>
 *   --profile-id <0..255>
 */
import { cancelOrderForRiskProfileInstruction } from '../src/instructions/index.js';
import {
  loadConnection,
  loadSigner,
  numberFlag,
  pubkeyFlag,
  runScript,
} from './_runner.js';

async function main(): Promise<void> {
  const conn = loadConnection();
  const signer = loadSigner();
  const ix = cancelOrderForRiskProfileInstruction({
    feePayer: signer.publicKey,
    curator: signer.publicKey,
    mint: pubkeyFlag('mint'),
    market: pubkeyFlag('market'),
    profileId: numberFlag('profile-id'),
  });
  await runScript({ conn, signer, ixs: [ix] });
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
