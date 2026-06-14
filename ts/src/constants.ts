import { PublicKey } from '@solana/web3.js';

/** Deployed program id of the yDelta program. */
export const YDELTA_PROGRAM_ID = new PublicKey(
  'HfYCWUgFuUbuzZTeQAAGzVkXw2uFM51QRoDfhFV8vyCj',
);

/** PDA seed prefixes — must stay byte-identical to the on-chain definitions. */
export const SEEDS = {
  marketSigner: Buffer.from('market_signer'),
  marginfiLenderAccount: Buffer.from('marginfi_account'),
  marginfiBorrowerAccount: Buffer.from('borrower_marginfi_account'),
  marketVault: Buffer.from('vault'),
  globalVault: Buffer.from('vault'),
  globalVaultSigner: Buffer.from('global_vault_signer'),
  globalVaultIntegration: Buffer.from('vault_integration'),
  globalVaultStaging: Buffer.from('global_vault_staging'),
  loan: Buffer.from('loan'),
  globalConfig: Buffer.from('global_config'),
  userAccount: Buffer.from('user'),
} as const;

/** Match-time per-instruction CU budget the heavy CPI-bearing ixs need. */
export const HEAVY_IX_CU_LIMIT = 600_000;

/** fp48 scale: amounts in the on-chain code are stored as `value << 48`. */
export const FP48_SHIFT = 48n;
export const FP48_ONE = 1n << FP48_SHIFT;

/** A sentinel index returned by the on-chain RB trees for "no node". */
export const NIL_DATA_INDEX = 0xffff_ffff;

/** Origination / liquidation fee scaling factor. */
export const BPS_PER_UNIT = 10_000n;
