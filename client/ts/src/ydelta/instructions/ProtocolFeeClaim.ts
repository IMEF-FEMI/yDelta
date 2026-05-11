import {
  AccountMeta,
  PublicKey,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import {
  globalConfigPda,
  lenderMarginfiAccountPda,
  marketSignerPda,
  marketVaultPda,
} from '../../utils/pdas';

export const protocolFeeClaimInstructionDiscriminator = YdeltaInstructionTag.ProtocolFeeClaim;

/** ProtocolFeeClaim drains accumulated protocol fee shares to the
 *  protocol admin's debt-token ATA. Signer must equal
 *  `GlobalConfig.protocol_admin` (NOT MarketFixed.admin). */
export type ProtocolFeeClaimInstructionAccounts = {
  /** Must equal `GlobalConfig.protocol_admin`. */
  protocolAdmin: PublicKey;
  market: PublicKey;
  debtMint: PublicKey;
  /** Debt-token ATA owned by `protocolAdmin`. */
  protocolAdminDebtToken: PublicKey;
  debtBank: PublicKey;
  debtLiquidityVault: PublicKey;
  debtBankLva: PublicKey;
  debtOracles: PublicKey[];
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
};

export function createProtocolFeeClaimInstruction(
  accounts: ProtocolFeeClaimInstructionAccounts,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [marketDebtVault] = marketVaultPda(accounts.market, accounts.debtMint, programId);
  const [marketSigner] = marketSignerPda(accounts.market, programId);
  const [lenderMarginfi] = lenderMarginfiAccountPda(accounts.market, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.protocolAdmin, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: accounts.protocolAdminDebtToken, isWritable: true, isSigner: false },
    { pubkey: marketDebtVault, isWritable: true, isSigner: false },
    { pubkey: marketSigner, isWritable: false, isSigner: false },
    { pubkey: lenderMarginfi, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBank, isWritable: true, isSigner: false },
    { pubkey: accounts.debtLiquidityVault, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBankLva, isWritable: false, isSigner: false },
  ];
  for (const o of accounts.debtOracles) {
    keys.push({ pubkey: o, isWritable: false, isSigner: false });
  }
  keys.push(
    { pubkey: accounts.debtMint, isWritable: false, isSigner: false },
    { pubkey: accounts.tokenProgram, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiGroup, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false },
  );
  return new TransactionInstruction({
    programId,
    keys,
    data: Buffer.from([protocolFeeClaimInstructionDiscriminator]),
  });
}
