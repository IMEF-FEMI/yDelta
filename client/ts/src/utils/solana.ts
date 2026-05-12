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

/**
 * Resolve the SPL token program (`spl-token` vs `spl-token-2022`)
 * a `mint` is registered under by reading its account owner. Returns
 * a fallback (default: legacy SPL Token) when the account is missing
 * or owned by an unexpected program — callers can rely on the lookup
 * to fail downstream when the bank actually requires Token-2022.
 *
 * Cached lookups are cheap; the result never changes for a given mint
 * (mints are immutable once initialized), so callers may memoize.
 */
export async function resolveMintTokenProgram(
  connection: Connection,
  mint: PublicKey,
  fallback: PublicKey = TOKEN_PROGRAM_ID,
): Promise<PublicKey> {
  const info = await connection.getAccountInfo(mint);
  if (!info) return fallback;
  if (info.owner.equals(TOKEN_PROGRAM_ID)) return TOKEN_PROGRAM_ID;
  if (info.owner.equals(TOKEN_2022_PROGRAM_ID)) return TOKEN_2022_PROGRAM_ID;
  return fallback;
}
