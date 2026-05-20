/**
 * `create-risk-profile.ts` — vault-admin allocates a new profile slot.
 *
 * Flags:
 *   --mint <PUBKEY>
 *   --profile-id <0..255>
 *   --curator <PUBKEY>
 *   --max-ltv <BPS>            (1..9999)
 *   --max-term-seconds <SECS>  (> 0)
 */
import { createRiskProfileInstruction } from '../src/instructions/index.js';
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
  const ix = createRiskProfileInstruction({
    payer: signer.publicKey,
    mint: pubkeyFlag('mint'),
    profileId: numberFlag('profile-id'),
    curator: pubkeyFlag('curator'),
    maxLtvBps: numberFlag('max-ltv'),
    maxTermSeconds: numberFlag('max-term-seconds'),
  });
  await runScript({ conn, signer, ixs: [ix] });
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
