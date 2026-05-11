import * as beet from '@metaplex-foundation/beet';
import {
  AccountMeta,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from '@solana/web3.js';
import BN from 'bn.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import { globalConfigPda, loanPda, splitLoanPda } from '../../utils/pdas';
import { ProcessMatchedLoanParams } from '../types/ProcessMatchedLoanParams';

export const processMatchedLoanInstructionDiscriminator = YdeltaInstructionTag.ProcessMatchedLoan;

/** Extra accounts required when the trigger seat is risk-profile-owned. */
export type VaultSettleAddrs = {
  globalVault: PublicKey;
  globalVaultSigner: PublicKey;
  globalVaultStaging: PublicKey;
  globalVaultIntegrationAccount: PublicKey;
  marketDebtVault: PublicKey;
  marketLenderIntegrationAccount: PublicKey;
  marketSigner: PublicKey;
  debtLiquidityVault: PublicKey;
  debtBankLiquidityVaultAuthority: PublicKey;
  /** Single oracle: `debt_bank.config.oracle_keys[0]`. Vault-settle skips the variadic
   *  staked-bank tail because the loader validates only `primary_oracle()`. */
  bankOracle: PublicKey;
  debtMint: PublicKey;
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
};

export type ProcessMatchedLoanInstructionAccounts = {
  payer: PublicKey;
  market: PublicKey;
  /** For primary or secondary-full, the loan PDA derived from the queue node's
   *  `referenced_loan_sequence` (secondary) or the queue node's `sequence` (primary).
   *  Caller is responsible for passing the right one. */
  loan: PublicKey;
  debtBank: PublicKey;
  collateralBank?: PublicKey; // present on secondary
  debtOracles?: PublicKey[]; // present on secondary
  collateralOracles?: PublicKey[]; // present on secondary
  marginfiProgram: PublicKey;
  vaultSettle?: VaultSettleAddrs;
  /** For SECONDARY|SPLIT mode, the freshly-derived split-loan PDA. */
  splitLoan?: PublicKey;
};

export type ProcessMatchedLoanInstructionArgs = ProcessMatchedLoanParams;

const ProcessMatchedLoanStruct = new beet.FixableBeetArgsStruct<
  ProcessMatchedLoanInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['matchedLoanSequence', beet.u64],
    ['matchedLoanIndexHint', beet.coption(beet.u32)],
  ],
  'ProcessMatchedLoanInstructionArgs',
);

export function createProcessMatchedLoanInstruction(
  accounts: ProcessMatchedLoanInstructionAccounts,
  args: ProcessMatchedLoanInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = ProcessMatchedLoanStruct.serialize({
    instructionDiscriminator: processMatchedLoanInstructionDiscriminator,
    ...args,
  });

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: accounts.loan, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBank, isWritable: true, isSigner: false },
    { pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
  ];

  const isSecondary = accounts.collateralBank !== undefined;
  if (isSecondary) {
    keys.push({ pubkey: accounts.collateralBank!, isWritable: false, isSigner: false });
    for (const o of accounts.debtOracles ?? []) {
      keys.push({ pubkey: o, isWritable: false, isSigner: false });
    }
    for (const o of accounts.collateralOracles ?? []) {
      keys.push({ pubkey: o, isWritable: false, isSigner: false });
    }
  }

  if (accounts.splitLoan) {
    keys.push({ pubkey: accounts.splitLoan, isWritable: true, isSigner: false });
  }

  if (accounts.vaultSettle) {
    const v = accounts.vaultSettle;
    keys.push(
      { pubkey: v.globalVault, isWritable: true, isSigner: false },
      { pubkey: v.globalVaultSigner, isWritable: false, isSigner: false },
      { pubkey: v.globalVaultStaging, isWritable: true, isSigner: false },
      { pubkey: v.globalVaultIntegrationAccount, isWritable: true, isSigner: false },
      { pubkey: v.marketDebtVault, isWritable: true, isSigner: false },
      { pubkey: v.marketLenderIntegrationAccount, isWritable: true, isSigner: false },
      { pubkey: v.marketSigner, isWritable: false, isSigner: false },
      { pubkey: v.debtLiquidityVault, isWritable: true, isSigner: false },
      { pubkey: v.debtBankLiquidityVaultAuthority, isWritable: false, isSigner: false },
      { pubkey: v.bankOracle, isWritable: false, isSigner: false },
      { pubkey: v.debtMint, isWritable: false, isSigner: false },
      { pubkey: v.tokenProgram, isWritable: false, isSigner: false },
      { pubkey: v.marginfiGroup, isWritable: false, isSigner: false },
      { pubkey: v.marginfiProgram, isWritable: false, isSigner: false },
    );
  }

  return new TransactionInstruction({ programId, keys, data });
}

/** Convenience: derive the loan PDA for a primary-promotion call. */
export function deriveLoanPdaForPrimary(
  market: PublicKey,
  sequence: BN | number | bigint,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): PublicKey {
  const seq = typeof sequence === 'bigint' ? sequence : BigInt(sequence.toString());
  return loanPda(market, seq, programId)[0];
}

/** Convenience: derive the split-loan PDA for a SECONDARY|SPLIT call. */
export function deriveSplitLoanPda(
  market: PublicKey,
  queueNodeSequence: BN | number | bigint,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): PublicKey {
  const seq = typeof queueNodeSequence === 'bigint' ? queueNodeSequence : BigInt(queueNodeSequence.toString());
  return splitLoanPda(market, seq, programId)[0];
}
