import {
  AccountMeta,
  PublicKey,
  TransactionInstruction,
} from '@solana/web3.js';
import BN from 'bn.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import {
  globalConfigPda,
  globalVaultSignerPda,
  loanPda,
  marketSignerPda,
  marketVaultPda,
  vaultIntegrationAccountPda,
  vaultStagingPda,
} from '../../utils/pdas';

export const claimRepaymentForRiskProfileInstructionDiscriminator = YdeltaInstructionTag.ClaimRepaymentForRiskProfile;

/** The vault-settle loader (`load_vault_settle_accounts`) reads exactly ONE
 *  oracle (`bank.config.primary_oracle()`), so this builder takes a single
 *  `bankOracle` rather than a variadic slice — passing more than one would
 *  misalign the trailing `mint, token_program, marginfi_group,
 *  marginfi_program` accounts. */
export type ClaimRepaymentForRiskProfileInstructionAccounts = {
  payer: PublicKey;
  market: PublicKey;
  sequence: BN | number | bigint;
  globalVault: PublicKey;
  debtMint: PublicKey;
  debtBank: PublicKey;
  debtLiquidityVault: PublicKey;
  debtBankLva: PublicKey;
  bankOracle: PublicKey;
  lenderMarginfiAccount: PublicKey;
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
  crankerRefund?: PublicKey;
};

export function createClaimRepaymentForRiskProfileInstruction(
  accounts: ClaimRepaymentForRiskProfileInstructionAccounts,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const seq =
    typeof accounts.sequence === 'bigint'
      ? accounts.sequence
      : BigInt(accounts.sequence.toString());
  const [loan] = loanPda(accounts.market, seq, programId);
  const [marketSigner] = marketSignerPda(accounts.market, programId);
  const [marketDebtVault] = marketVaultPda(accounts.market, accounts.debtMint, programId);
  const [globalVaultSigner] = globalVaultSignerPda(accounts.globalVault, programId);
  const [globalVaultStaging] = vaultStagingPda(accounts.globalVault, programId);
  const [globalVaultIntegration] = vaultIntegrationAccountPda(accounts.globalVault, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: loan, isWritable: true, isSigner: false },
    { pubkey: accounts.globalVault, isWritable: true, isSigner: false },
    { pubkey: globalVaultSigner, isWritable: false, isSigner: false },
    { pubkey: globalVaultStaging, isWritable: true, isSigner: false },
    { pubkey: globalVaultIntegration, isWritable: true, isSigner: false },
    { pubkey: marketDebtVault, isWritable: true, isSigner: false },
    { pubkey: marketSigner, isWritable: false, isSigner: false },
    { pubkey: accounts.lenderMarginfiAccount, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBank, isWritable: true, isSigner: false },
    { pubkey: accounts.debtLiquidityVault, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBankLva, isWritable: false, isSigner: false },
    { pubkey: accounts.bankOracle, isWritable: false, isSigner: false },
    { pubkey: accounts.debtMint, isWritable: false, isSigner: false },
    { pubkey: accounts.tokenProgram, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiGroup, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false },
  ];
  if (accounts.crankerRefund) {
    keys.push({ pubkey: accounts.crankerRefund, isWritable: true, isSigner: false });
  }
  return new TransactionInstruction({
    programId,
    keys,
    data: Buffer.from([claimRepaymentForRiskProfileInstructionDiscriminator]),
  });
}
