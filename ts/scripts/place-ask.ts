/**
 * `place-ask.ts` — curator places a vault ask (zero-CPI). Same signer pays
 * fees AND curates by default; pass `--fee-payer <PUBKEY>` to split.
 *
 * Flags:
 *   --mint <PUBKEY>
 *   --market <PUBKEY>
 *   --profile-id <0..255>
 *   --rate-bps <BPS>
 *   --term-seconds <SECS>
 *   --flags <U8>               (default 0)
 *   --fee-payer <PUBKEY>       (optional — default = signer)
 */
import { placeOrderForRiskProfileInstruction } from '../src/instructions/index.js';
import {
  flag,
  loadConnection,
  loadSigner,
  numberFlag,
  optionalPubkeyFlag,
  pubkeyFlag,
  runScript,
} from './_runner.js';

async function main(): Promise<void> {
  const conn = loadConnection();
  const signer = loadSigner();
  const feePayer = optionalPubkeyFlag('fee-payer') ?? signer.publicKey;
  const flagsStr = flag('flags');
  const ix = placeOrderForRiskProfileInstruction({
    feePayer,
    curator: signer.publicKey,
    mint: pubkeyFlag('mint'),
    market: pubkeyFlag('market'),
    profileId: numberFlag('profile-id'),
    rateBps: numberFlag('rate-bps'),
    termSeconds: numberFlag('term-seconds'),
    flags: flagsStr === undefined ? 0 : Number(flagsStr),
  });
  await runScript({ conn, signer, ixs: [ix] });
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
