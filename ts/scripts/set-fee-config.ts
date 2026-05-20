/**
 * `set-fee-config.ts` — admin updates the market's fee config. Each flag
 * is independent; omit a field to leave it unchanged on-chain.
 *
 * Flags:
 *   --market <PUBKEY>
 *   --protocol-fee-floor <BPS>
 *   --origination <BPS>
 *   --curator-split <BPS>
 *   --curator-fee <BPS>
 *   --liquidation-keeper <BPS>
 *   --liquidation-protocol <BPS>
 *   --ltv-buffer <BPS>
 *   --grace-period <SECS>
 */
import { setFeeConfigInstruction } from '../src/instructions/index.js';
import { flag, loadConnection, loadSigner, pubkeyFlag, runScript } from './_runner.js';

function optionalNumber(name: string): number | undefined {
  const v = flag(name);
  return v === undefined ? undefined : Number(v);
}

async function main(): Promise<void> {
  const conn = loadConnection();
  const signer = loadSigner();

  const ix = setFeeConfigInstruction({
    admin: signer.publicKey,
    market: pubkeyFlag('market'),
    protocolFeeBpsFloor: optionalNumber('protocol-fee-floor'),
    originationBps: optionalNumber('origination'),
    curatorSplitBps: optionalNumber('curator-split'),
    curatorFeeBps: optionalNumber('curator-fee'),
    liquidationKeeperBps: optionalNumber('liquidation-keeper'),
    liquidationProtocolBps: optionalNumber('liquidation-protocol'),
    ltvBufferBps: optionalNumber('ltv-buffer'),
    gracePeriodSeconds: optionalNumber('grace-period'),
  });
  await runScript({ conn, signer, ixs: [ix] });
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
