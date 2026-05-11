import { PublicKey } from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from './programId';

export const GLOBAL_CONFIG_SEED = Buffer.from('global_config');
export const MARKET_SIGNER_SEED = Buffer.from('market_signer');
export const MARGINFI_LENDER_ACCOUNT_SEED = Buffer.from('marginfi_account');
export const MARGINFI_BORROWER_ACCOUNT_SEED = Buffer.from(
  'borrower_marginfi_account',
);
export const MARKET_VAULT_SEED = Buffer.from('vault');
export const LOAN_SEED = Buffer.from('loan');
export const SPLIT_LOAN_SEED = Buffer.from('split_loan');
export const USER_ACCOUNT_SEED = Buffer.from('user');
export const GLOBAL_VAULT_SEED = Buffer.from('vault');
export const GLOBAL_VAULT_SIGNER_SEED = Buffer.from('global_vault_signer');
export const VAULT_INTEGRATION_SEED = Buffer.from('vault_integration');
export const VAULT_STAGING_SEED = Buffer.from('global_vault_staging');

function find(seeds: Buffer[], programId: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(seeds, programId);
}

export function globalConfigPda(
  programId: PublicKey = YDELTA_PROGRAM_ID,
): [PublicKey, number] {
  return find([GLOBAL_CONFIG_SEED], programId);
}

export function marketSignerPda(
  market: PublicKey,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): [PublicKey, number] {
  return find([MARKET_SIGNER_SEED, market.toBuffer()], programId);
}

export function lenderMarginfiAccountPda(
  market: PublicKey,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): [PublicKey, number] {
  return find([MARGINFI_LENDER_ACCOUNT_SEED, market.toBuffer()], programId);
}

export function borrowerMarginfiAccountPda(
  market: PublicKey,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): [PublicKey, number] {
  return find([MARGINFI_BORROWER_ACCOUNT_SEED, market.toBuffer()], programId);
}

/** Per-market staging SPL vault for a given mint. Seeds: [b"vault", market, mint]. */
export function marketVaultPda(
  market: PublicKey,
  mint: PublicKey,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): [PublicKey, number] {
  return find([MARKET_VAULT_SEED, market.toBuffer(), mint.toBuffer()], programId);
}

function u64LeBytes(n: bigint | number): Buffer {
  const buf = Buffer.alloc(8);
  buf.writeBigUInt64LE(typeof n === 'bigint' ? n : BigInt(n));
  return buf;
}

export function loanPda(
  market: PublicKey,
  sequence: bigint | number,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): [PublicKey, number] {
  return find([LOAN_SEED, market.toBuffer(), u64LeBytes(sequence)], programId);
}

export function splitLoanPda(
  market: PublicKey,
  queueNodeSequence: bigint | number,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): [PublicKey, number] {
  return find(
    [SPLIT_LOAN_SEED, market.toBuffer(), u64LeBytes(queueNodeSequence)],
    programId,
  );
}

export function userAccountPda(
  owner: PublicKey,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): [PublicKey, number] {
  return find([USER_ACCOUNT_SEED, owner.toBuffer()], programId);
}

/** Global vault (per-mint). Seeds: [b"vault", mint] — note the 2-seed shape
 *  distinguishes it from per-market staging vaults which use 3 seeds. */
export function globalVaultPda(
  mint: PublicKey,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): [PublicKey, number] {
  return find([GLOBAL_VAULT_SEED, mint.toBuffer()], programId);
}

export function globalVaultSignerPda(
  vault: PublicKey,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): [PublicKey, number] {
  return find([GLOBAL_VAULT_SIGNER_SEED, vault.toBuffer()], programId);
}

export function vaultIntegrationAccountPda(
  vault: PublicKey,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): [PublicKey, number] {
  return find([VAULT_INTEGRATION_SEED, vault.toBuffer()], programId);
}

export function vaultStagingPda(
  vault: PublicKey,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): [PublicKey, number] {
  return find([VAULT_STAGING_SEED, vault.toBuffer()], programId);
}
