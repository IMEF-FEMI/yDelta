/**
 * claim-curator-fee.ts — `ClaimCuratorFee` (tag 15). Curator sweeps
 * accumulated fees from a sub-vault to their ATA. No-op if zero.
 *
 * Reads:
 *   .local/claim-curator-fee-input.json {
 *     mint: string,
 *     subVaultId: number,
 *     curatorTokenAta: string
 *   }
 *   .local/vaults.json, .local/sub-vaults.json, .local/curators.json
 */
import { PublicKey } from '@solana/web3.js';

import { claimCuratorFeeInstruction } from '../src/instructions/index.js';
import {
  appendTxLog,
  loadConnection,
  log,
  readJson,
  readKeypairFromBase58,
  sendIxs,
} from './_runner.js';
import {
  bankLiquidityVaultAuthority,
  MARGINFI_PROGRAM_ID,
  readBankOnchain,
  resolveBankByMint,
} from './_marginfi.js';
import { crankStaleBankOracles } from './_oracleCrank.js';
import type { CuratorDump, SubVaultDump, VaultDump } from './_types.js';
import { resolveCuratorForSubVault } from './_types.js';

interface Input {
  mint: string;
  subVaultId: number;
  curatorTokenAta: string;
}

async function main(): Promise<void> {
  const input = readJson<Input>('claim-curator-fee-input.json');
  const vaults = readJson<Record<string, VaultDump>>('vaults.json');
  const subVaults = readJson<Record<string, SubVaultDump[]>>('sub-vaults.json');
  const curators = readJson<CuratorDump[]>('curators.json');

  const vault = vaults[input.mint];
  if (!vault) throw new Error(`vaults.json: no vault for mint ${input.mint}`);
  const curatorEntry = resolveCuratorForSubVault(subVaults, curators, input.mint, input.subVaultId);
  const curator = readKeypairFromBase58(curatorEntry.secretKeyBase58);

  const conn = loadConnection();
  const bankPk = new PublicKey(vault.bank);
  const bank = await readBankOnchain(conn, bankPk);
  const bankEntry = resolveBankByMint(input.mint);
  const crank = await crankStaleBankOracles(conn, curator, [
    { bank, pythFeedIdHex: bankEntry.pythFeedIdHex, pythShardId: bankEntry.pythShardId },
  ]);

  log(`[claim-curator-fee] curator=${curator.publicKey.toBase58()} subVaultId=${input.subVaultId}`);

  const ix = claimCuratorFeeInstruction({
    curator: curator.publicKey,
    // v1: the vault PDA is bank-keyed (`[b"vault", bank]`); the mint is still
    // passed as a readonly account for the marginfi withdraw path.
    mint: new PublicKey(input.mint),
    subVaultId: input.subVaultId,
    curatorToken: new PublicKey(input.curatorTokenAta),
    debtBank: bankPk,
    liquidityVault: bank.liquidityVault,
    bankLiquidityVaultAuthority: bankLiquidityVaultAuthority(bankPk),
    bankOracle: bank.oracleKeys[0],
    marginfiProgram: MARGINFI_PROGRAM_ID,
    marginfiGroup: new PublicKey(vault.group),
  });
  const sig = await sendIxs(conn, curator, [ix]);
  log(`[claim-curator-fee] signature = ${sig}`);
  appendTxLog({
    script: 'claim-curator-fee',
    signatures: [sig],
    oracleCrank: crank.entries,
    summary: {
      mint: input.mint,
      subVaultId: input.subVaultId,
      curator: curator.publicKey.toBase58(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
