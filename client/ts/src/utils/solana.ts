import {
  Connection,
  PublicKey,
  TransactionInstruction,
} from '@solana/web3.js';
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
  createAssociatedTokenAccountInstruction,
  getAssociatedTokenAddressSync,
} from '@solana/spl-token';

export {
  TOKEN_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
  ASSOCIATED_TOKEN_PROGRAM_ID,
  getAssociatedTokenAddressSync,
};

export type CreateAtaIfMissingResult = {
  ata: PublicKey;
  /** `null` when the ATA already exists; otherwise the create-ATA ix to prepend. */
  createIx: TransactionInstruction | null;
};

export async function createAtaIfMissing(
  connection: Connection,
  payer: PublicKey,
  owner: PublicKey,
  mint: PublicKey,
  tokenProgram: PublicKey = TOKEN_PROGRAM_ID,
): Promise<CreateAtaIfMissingResult> {
  const ata = getAssociatedTokenAddressSync(
    mint,
    owner,
    /*allowOwnerOffCurve=*/ true,
    tokenProgram,
  );
  const info = await connection.getAccountInfo(ata);
  if (info) return { ata, createIx: null };
  return {
    ata,
    createIx: createAssociatedTokenAccountInstruction(
      payer,
      ata,
      owner,
      mint,
      tokenProgram,
    ),
  };
}

export function deriveAta(
  owner: PublicKey,
  mint: PublicKey,
  tokenProgram: PublicKey = TOKEN_PROGRAM_ID,
): PublicKey {
  return getAssociatedTokenAddressSync(mint, owner, true, tokenProgram);
}
