/**
 * vault-deposit.ts — `GlobalVaultDeposit` (tag 10). Depositor LP into the
 * USDC global vault under a specific sub-vault.
 *
 * Reads:
 *   .local/vault-deposit-input.json {
 *     mint: string,                          // debt mint of the vault
 *     depositorKeypairPath?: string,         // path; defaults to signer
 *     depositorTokenAta: string,             // depositor's mint ATA
 *     subVaultId: number,                    // u16 (v1; was profileId u8)
 *     amountAtoms: string | number
 *   }
 *   .local/vaults.json
 *
 * Deposit doesn't read oracles, so no crank — atoms route via the
 * staging vault into the bank and back as freshly-minted vault shares.
 */
import { PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import { globalVaultDepositInstruction } from '../src/instructions/index.js';
import {
  appendTxLog,
  ataFor,
  createAtaIdempotentIx,
  loadConnection,
  loadDepositor,
  log,
  readJson,
  sendIxs,
} from './_runner.js';
import { MARGINFI_PROGRAM_ID } from './_marginfi.js';
import type { VaultDump } from './_types.js';

interface Input {
  mint: string;
  subVaultId: number;
  amountAtoms: string | number;
}

async function main(): Promise<void> {
  const input = readJson<Input>('vault-deposit-input.json');
  const vaults = readJson<Record<string, VaultDump>>('vaults.json');
  const vault = vaults[input.mint];
  if (!vault) throw new Error(`vaults.json: no vault for mint ${input.mint}`);

  const conn = loadConnection();
  const depositor = loadDepositor();
  const mint = new PublicKey(input.mint);
  const depositorToken = ataFor(depositor.publicKey, mint);

  const amount = BigInt(input.amountAtoms.toString());
  log(`[vault-deposit] mint=${input.mint} subVaultId=${input.subVaultId} amount=${amount} depositor=${depositor.publicKey.toBase58()}`);

  const ix = globalVaultDepositInstruction({
    depositor: depositor.publicKey,
    mint,
    depositorToken,
    tokenProgram: TOKEN_PROGRAM_ID,
    marginfiGroup: new PublicKey(vault.group),
    lendingPool: new PublicKey(vault.bank),
    liquidityVault: new PublicKey(await vaultLiquidityVault(conn, vault.bank)),
    marginfiProgram: MARGINFI_PROGRAM_ID,
    subVaultId: input.subVaultId,
    amountAtoms: amount,
  });
  // Idempotently ensure the depositor's source ATA exists (must already hold
  // the atoms being deposited).
  const sig = await sendIxs(conn, depositor, [
    createAtaIdempotentIx(depositor.publicKey, depositor.publicKey, mint),
    ix,
  ]);
  log(`[vault-deposit] signature = ${sig}`);
  appendTxLog({
    script: 'vault-deposit',
    signatures: [sig],
    summary: {
      mint: input.mint,
      subVaultId: input.subVaultId,
      depositor: depositor.publicKey.toBase58(),
      amountAtoms: amount.toString(),
    },
  });
}

async function vaultLiquidityVault(
  conn: ReturnType<typeof loadConnection>,
  bankAddr: string,
): Promise<string> {
  // The vault's pinned bank's liquidityVault is exposed via the bank's
  // own state — we re-read it here to avoid coupling to whatever
  // `vaults.json` happened to capture at vault-create time (which may
  // become stale if marginfi-ops rotates the vault).
  const { readBankOnchain } = await import('./_marginfi.js');
  const bank = await readBankOnchain(conn, new PublicKey(bankAddr));
  return bank.liquidityVault.toBase58();
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
