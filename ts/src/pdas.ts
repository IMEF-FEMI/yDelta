import { PublicKey } from '@solana/web3.js';
import BN from 'bn.js';

import { SEEDS, YDELTA_PROGRAM_ID } from './constants.js';

/** `[market_signer, market]` — authority for marginfi and SPL-vault CPIs. */
export function marketSignerPda(market: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.marketSigner, market.toBuffer()],
    YDELTA_PROGRAM_ID,
  );
}

/** `[marginfi_account, market]` — lender-side marginfi account. */
export function lenderIntegrationAccountPda(market: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.marginfiLenderAccount, market.toBuffer()],
    YDELTA_PROGRAM_ID,
  );
}

/** `[borrower_marginfi_account, market]` — borrower-side marginfi account. */
export function borrowerIntegrationAccountPda(market: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.marginfiBorrowerAccount, market.toBuffer()],
    YDELTA_PROGRAM_ID,
  );
}

/** `[vault, market, mint]` — per-market SPL-token vault. */
export function marketTokenVaultPda(
  market: PublicKey,
  mint: PublicKey,
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.marketVault, market.toBuffer(), mint.toBuffer()],
    YDELTA_PROGRAM_ID,
  );
}

/** `[vault, bank]` — GlobalVault PDA (one per marginfi bank / debt lending pool). */
export function globalVaultPda(bank: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.globalVault, bank.toBuffer()],
    YDELTA_PROGRAM_ID,
  );
}

/** `[global_vault_signer, vault]` — authority for vault marginfi/SPL CPIs. */
export function globalVaultSignerPda(vault: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.globalVaultSigner, vault.toBuffer()],
    YDELTA_PROGRAM_ID,
  );
}

/** `[vault_integration, vault]` — vault's marginfi-account PDA. */
export function globalVaultIntegrationAccountPda(vault: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.globalVaultIntegration, vault.toBuffer()],
    YDELTA_PROGRAM_ID,
  );
}

/** `[global_vault_staging, vault]` — SPL staging vault for deposit/withdraw hop. */
export function globalVaultStagingPda(vault: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.globalVaultStaging, vault.toBuffer()],
    YDELTA_PROGRAM_ID,
  );
}

/** `[loan, market, sequence_le_bytes]` — per-loan PDA. */
export function loanPda(market: PublicKey, sequence: bigint | BN): [PublicKey, number] {
  const seqLe = Buffer.alloc(8);
  const seqBn = typeof sequence === 'bigint' ? new BN(sequence.toString()) : sequence;
  seqBn.toArrayLike(Buffer, 'le', 8).copy(seqLe);
  return PublicKey.findProgramAddressSync(
    [SEEDS.loan, market.toBuffer(), seqLe],
    YDELTA_PROGRAM_ID,
  );
}

/** `[global_config]` — singleton protocol-config PDA. */
export function globalConfigPda(): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([SEEDS.globalConfig], YDELTA_PROGRAM_ID);
}

/** `[user, owner]` — per-wallet UserAccount PDA. */
export function userAccountPda(owner: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.userAccount, owner.toBuffer()],
    YDELTA_PROGRAM_ID,
  );
}
