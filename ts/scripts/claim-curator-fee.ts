/**
 * `claim-curator-fee.ts` — drains the profile's curator fee accumulator
 * to the curator's wallet ATA. No-op (Ok) on zero, so safe to poll.
 *
 * Flags:
 *   --mint <PUBKEY>
 *   --profile-id <0..255>
 *   --curator-token <PUBKEY>
 *   --debt-bank <PUBKEY>
 *   --liquidity-vault <PUBKEY>
 *   --bank-lva <PUBKEY>
 *   --bank-oracle <PUBKEY>
 *   --marginfi-program <PUBKEY>
 *   --marginfi-group <PUBKEY>
 */
import { claimCuratorFeeInstruction } from '../src/instructions/index.js';
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
  const ix = claimCuratorFeeInstruction({
    curator: signer.publicKey,
    mint: pubkeyFlag('mint'),
    profileId: numberFlag('profile-id'),
    curatorToken: pubkeyFlag('curator-token'),
    debtBank: pubkeyFlag('debt-bank'),
    liquidityVault: pubkeyFlag('liquidity-vault'),
    bankLiquidityVaultAuthority: pubkeyFlag('bank-lva'),
    bankOracle: pubkeyFlag('bank-oracle'),
    marginfiProgram: pubkeyFlag('marginfi-program'),
    marginfiGroup: pubkeyFlag('marginfi-group'),
  });
  await runScript({ conn, signer, ixs: [ix] });
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
