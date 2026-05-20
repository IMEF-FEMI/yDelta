/**
 * `crank-matched-loan.ts` — promotes a `MatchedLoan` queue node into a
 * `LoanFixed` PDA. Heavy when the lender is a vault (vault → market drain
 * via marginfi CPI), so we always prepend the 600 000 CU limit.
 *
 * Required flags:
 *   --market <PUBKEY>
 *   --sequence <U64>
 *   --debt-bank <PUBKEY>
 *   --marginfi-program <PUBKEY>
 *
 * Optional — pass ALL of these together for a vault-lender match:
 *   --debt-mint, --debt-liquidity-vault, --debt-bank-lva,
 *   --debt-oracles <PUBKEY>[,<PUBKEY>...],
 *   --token-program, --marginfi-group
 */
import { PublicKey } from '@solana/web3.js';

import {
  HEAVY_IX_CU_LIMIT,
  globalVaultIntegrationAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
  lenderIntegrationAccountPda,
  marketSignerPda,
  marketTokenVaultPda,
  processMatchedLoanInstruction,
} from '../src/index.js';
import {
  bigintFlag,
  flag,
  loadConnection,
  loadSigner,
  optionalPubkeyFlag,
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
  const market = pubkeyFlag('market');

  const debtMint = optionalPubkeyFlag('debt-mint');
  const vaultSettle = debtMint
    ? (() => {
        const vault = globalVaultPda(debtMint)[0];
        return {
          globalVault: vault,
          globalVaultSigner: globalVaultSignerPda(vault)[0],
          globalVaultStaging: globalVaultStagingPda(vault)[0],
          globalVaultIntegrationAccount: globalVaultIntegrationAccountPda(vault)[0],
          marketDebtVault: marketTokenVaultPda(market, debtMint)[0],
          marketLenderIntegrationAccount: lenderIntegrationAccountPda(market)[0],
          marketSigner: marketSignerPda(market)[0],
          debtLiquidityVault: pubkeyFlag('debt-liquidity-vault'),
          debtBankLiquidityVaultAuthority: pubkeyFlag('debt-bank-lva'),
          debtOracles: csvPubkeys('debt-oracles'),
          debtMint,
          tokenProgram: pubkeyFlag('token-program'),
          marginfiGroup: pubkeyFlag('marginfi-group'),
          marginfiProgram: pubkeyFlag('marginfi-program'),
        };
      })()
    : undefined;

  const ix = processMatchedLoanInstruction({
    payer: signer.publicKey,
    market,
    debtBank: pubkeyFlag('debt-bank'),
    marginfiProgram: pubkeyFlag('marginfi-program'),
    sequence: bigintFlag('sequence'),
    vaultSettle,
  });
  await runScript({ conn, signer, ixs: [ix], cuLimit: HEAVY_IX_CU_LIMIT });
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
