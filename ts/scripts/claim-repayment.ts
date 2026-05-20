/**
 * `claim-repayment.ts` — permissionless cranker that closes a fully-repaid
 * risk-profile loan. Heavy (3 CPIs + oracle read). The vault-settle loader
 * reads exactly ONE oracle (`bank.config.primary_oracle()`), so the script
 * takes a single `--bank-oracle`.
 *
 * Flags:
 *   --market <PUBKEY>
 *   --sequence <U64>
 *   --global-vault <PUBKEY>
 *   --debt-mint <PUBKEY>
 *   --debt-bank <PUBKEY>
 *   --debt-liquidity-vault <PUBKEY>
 *   --debt-bank-lva <PUBKEY>
 *   --bank-oracle <PUBKEY>
 *   --lender-marginfi <PUBKEY>
 *   --token-program <PUBKEY>
 *   --marginfi-group <PUBKEY>
 *   --marginfi-program <PUBKEY>
 *   --cranker-refund <PUBKEY>        (required — must equal LoanPDA.created_by)
 */
import {
  HEAVY_IX_CU_LIMIT,
  claimRepaymentForRiskProfileInstruction,
} from '../src/index.js';
import {
  bigintFlag,
  loadConnection,
  loadSigner,
  pubkeyFlag,
  runScript,
} from './_runner.js';

async function main(): Promise<void> {
  const conn = loadConnection();
  const signer = loadSigner();
  const ix = claimRepaymentForRiskProfileInstruction({
    payer: signer.publicKey,
    market: pubkeyFlag('market'),
    sequence: bigintFlag('sequence'),
    globalVault: pubkeyFlag('global-vault'),
    debtMint: pubkeyFlag('debt-mint'),
    debtBank: pubkeyFlag('debt-bank'),
    debtLiquidityVault: pubkeyFlag('debt-liquidity-vault'),
    debtBankLiquidityVaultAuthority: pubkeyFlag('debt-bank-lva'),
    bankOracle: pubkeyFlag('bank-oracle'),
    lenderMarginfiAccount: pubkeyFlag('lender-marginfi'),
    tokenProgram: pubkeyFlag('token-program'),
    marginfiGroup: pubkeyFlag('marginfi-group'),
    marginfiProgram: pubkeyFlag('marginfi-program'),
    crankerRefund: pubkeyFlag('cranker-refund'),
  });
  await runScript({ conn, signer, ixs: [ix], cuLimit: HEAVY_IX_CU_LIMIT });
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
