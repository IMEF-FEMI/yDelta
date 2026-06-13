/**
 * vault-withdraw.ts — `GlobalVaultWithdraw` (tag 11). Depositor burns
 * vault shares for atoms (or, with `withdrawAll: true`, drains the full
 * redeemable balance under the chosen sub-vault).
 *
 * Reads:
 *   .local/vault-withdraw-input.json {
 *     mint: string,
 *     depositorKeypairPath?: string,
 *     depositorTokenAta: string,
 *     subVaultId: number,                    // u16 (v1; was profileId u8)
 *     sharesToBurn?: string | number,        // u128; required unless withdrawAll
 *     withdrawAll?: boolean                  // sets sharesToBurn to the on-chain stake
 *   }
 *   .local/vaults.json
 *
 * Withdraw runs a marginfi health check, so we read the bank's primary
 * oracle and crank it if stale. The ix itself reads only the vault's
 * pinned bank (single oracle), so we crank exactly that one.
 */
import { PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import { globalVaultWithdrawInstruction } from '../src/instructions/index.js';
import { decodeGlobalVault } from '../src/accounts/vault.js';
import { globalVaultPda } from '../src/pdas.js';
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
import {
  bankLiquidityVaultAuthority,
  MARGINFI_PROGRAM_ID,
  readBankOnchain,
  resolveBankByMint,
} from './_marginfi.js';
import { crankStaleBankOracles } from './_oracleCrank.js';
import type { VaultDump } from './_types.js';

interface Input {
  mint: string;
  subVaultId: number;
  sharesToBurn?: string | number;
  withdrawAll?: boolean;
}

async function main(): Promise<void> {
  const input = readJson<Input>('vault-withdraw-input.json');
  const vaults = readJson<Record<string, VaultDump>>('vaults.json');
  const vault = vaults[input.mint];
  if (!vault) throw new Error(`vaults.json: no vault for mint ${input.mint}`);

  const conn = loadConnection();
  const depositor = loadDepositor();
  const mint = new PublicKey(input.mint);
  const depositorToken = ataFor(depositor.publicKey, mint);

  const bankPk = new PublicKey(vault.bank);
  const bank = await readBankOnchain(conn, bankPk);

  let shares: bigint;
  if (input.withdrawAll) {
    shares = await readDepositorShares(conn, bankPk, depositor.publicKey, input.subVaultId);
    if (shares === 0n) {
      log(`[vault-withdraw] withdrawAll: depositor has 0 shares; nothing to do`);
      return;
    }
    log(`[vault-withdraw] withdrawAll: depositor holds ${shares} shares`);
  } else {
    if (input.sharesToBurn === undefined) {
      throw new Error('vault-withdraw-input.json: either sharesToBurn or withdrawAll must be set');
    }
    shares = BigInt(input.sharesToBurn.toString());
  }

  const bankEntry = resolveBankByMint(input.mint);
  const crank = await crankStaleBankOracles(conn, depositor, [
    { bank, pythFeedIdHex: bankEntry.pythFeedIdHex, pythShardId: bankEntry.pythShardId },
  ]);

  const ix = globalVaultWithdrawInstruction({
    depositor: depositor.publicKey,
    mint,
    depositorToken,
    tokenProgram: TOKEN_PROGRAM_ID,
    marginfiGroup: new PublicKey(vault.group),
    lendingPool: bankPk,
    lendingPoolOracle: bank.oracleKeys[0],
    liquidityVault: bank.liquidityVault,
    bankLiquidityVaultAuthority: bankLiquidityVaultAuthority(bankPk),
    marginfiProgram: MARGINFI_PROGRAM_ID,
    subVaultId: input.subVaultId,
    sharesToBurn: shares,
  });
  // Ensure the payout destination ATA exists before the withdraw lands.
  const sig = await sendIxs(conn, depositor, [
    createAtaIdempotentIx(depositor.publicKey, depositor.publicKey, mint),
    ix,
  ]);
  log(`[vault-withdraw] signature = ${sig}`);
  appendTxLog({
    script: 'vault-withdraw',
    signatures: [sig],
    oracleCrank: crank.entries,
    summary: {
      mint: input.mint,
      subVaultId: input.subVaultId,
      depositor: depositor.publicKey.toBase58(),
      sharesBurned: shares.toString(),
      withdrawAll: input.withdrawAll ?? false,
    },
  });
}

/**
 * Sum the depositor's share-balance across every per-(depositor, subVaultId)
 * slot in the vault. `decodeGlobalVault` decodes the dynamic region into
 * a list of `SubVaultDepositorSeat` entries; one depositor can have
 * multiple slots open under different sub-vaults (or repeat entries after
 * partial redemptions), so we filter to (owner, subVaultId) and sum.
 *
 * The GlobalVault PDA is bank-keyed in v1 (`[b"vault", bank]`), so we
 * derive it from the vault's pinned bank, NOT the mint.
 */
async function readDepositorShares(
  conn: ReturnType<typeof loadConnection>,
  bank: PublicKey,
  depositor: PublicKey,
  subVaultId: number,
): Promise<bigint> {
  const [vaultPk] = globalVaultPda(bank);
  const info = await conn.getAccountInfo(vaultPk);
  if (!info) throw new Error(`vault account ${vaultPk.toBase58()} not found on chain`);
  const vault = decodeGlobalVault(info.data);
  let total = 0n;
  for (const { seat } of vault.depositorSeats) {
    if (seat.subVaultId === subVaultId && seat.owner.equals(depositor)) {
      total += seat.shares;
    }
  }
  return total;
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
