/**
 * `init-market.ts` — one-shot `CreateMarket`. Signer becomes the market admin.
 *
 * The market account itself must be allocated by a separate
 * `SystemProgram.createAccount` ix at 512 bytes, owned by the yDelta program.
 * This script expects `--market <PUBKEY>` to already exist + be funded; the
 * caller is responsible for the upstream `createAccount`.
 *
 * Required flags:
 *   --market <PUBKEY>          (pre-allocated 512-byte program-owned account)
 *   --debt-mint <PUBKEY>
 *   --collateral-mint <PUBKEY>
 *   --marginfi-group <PUBKEY>
 *   --debt-bank <PUBKEY>
 *   --collateral-bank <PUBKEY>
 *   --marginfi-program <PUBKEY>
 */
import { createMarketInstruction } from '../src/instructions/index.js';
import { loadConnection, loadSigner, pubkeyFlag, runScript } from './_runner.js';

async function main(): Promise<void> {
  const conn = loadConnection();
  const signer = loadSigner();

  const ix = createMarketInstruction({
    marketCreator: signer.publicKey,
    market: pubkeyFlag('market'),
    debtMint: pubkeyFlag('debt-mint'),
    collateralMint: pubkeyFlag('collateral-mint'),
    marginfiGroup: pubkeyFlag('marginfi-group'),
    debtBank: pubkeyFlag('debt-bank'),
    collateralBank: pubkeyFlag('collateral-bank'),
    marginfiProgram: pubkeyFlag('marginfi-program'),
  });
  await runScript({ conn, signer, ixs: [ix] });
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
