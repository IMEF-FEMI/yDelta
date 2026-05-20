/**
 * `liquidate.ts` — LTV-gated liquidation. Heavy — 600k CU prepended.
 * `--repay-atoms-max 0` means "full repay". Reverts with `LoanStillSolvent`
 * when current LTV is below maintenance. Use `simulateLiquidate` (the
 * `CheckLtvLiquidatable` ix, via `simulateTransaction`) first for a cheap
 * pre-flight check.
 */
import { PublicKey } from '@solana/web3.js';

import { HEAVY_IX_CU_LIMIT, liquidateLoanInstruction } from '../src/index.js';
import {
  bigintFlag,
  flag,
  loadConnection,
  loadSigner,
  pubkeyFlag,
  runScript,
} from './_runner.js';

function csvPubkeys(name: string): PublicKey[] {
  const v = flag(name);
  if (!v) return [];
  return v.split(',').map((s) => new PublicKey(s.trim()));
}

async function main(): Promise<void> {
  const conn = loadConnection();
  const signer = loadSigner();
  const ix = liquidateLoanInstruction({
    payer: signer.publicKey,
    market: pubkeyFlag('market'),
    sequence: bigintFlag('sequence'),
    debtMint: pubkeyFlag('debt-mint'),
    collateralMint: pubkeyFlag('collateral-mint'),
    liquidatorDebtToken: pubkeyFlag('liquidator-debt-token'),
    liquidatorCollateralToken: pubkeyFlag('liquidator-collateral-token'),
    debtBank: pubkeyFlag('debt-bank'),
    collateralBank: pubkeyFlag('collateral-bank'),
    debtLiquidityVault: pubkeyFlag('debt-liquidity-vault'),
    collateralLiquidityVault: pubkeyFlag('collateral-liquidity-vault'),
    collateralBankLiquidityVaultAuthority: pubkeyFlag('collateral-bank-lva'),
    debtOracles: csvPubkeys('debt-oracles'),
    collateralOracles: csvPubkeys('collateral-oracles'),
    tokenProgram: pubkeyFlag('token-program'),
    marginfiGroup: pubkeyFlag('marginfi-group'),
    marginfiProgram: pubkeyFlag('marginfi-program'),
    repayAtomsMax: bigintFlag('repay-atoms-max'),
    crankerRefund: pubkeyFlag('cranker-refund'),
  });
  await runScript({ conn, signer, ixs: [ix], cuLimit: HEAVY_IX_CU_LIMIT });
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
