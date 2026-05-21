/**
 * crank-oracle.ts — refresh every oracle backing the live vault + markets.
 *
 * Collects the distinct marginfi banks referenced by `.local/vaults.json`
 * (the vault's pinned debt bank) and `.local/markets.json` (each market's
 * debt + collateral bank), reads each bank on-chain, and cranks any whose
 * primary oracle is past `bank.oracle_max_age`:
 *   - Pyth-Push  → posts the latest Hermes VAA to the canonical shard PDA.
 *   - Switchboard-Pull → fetches + posts a fresh pull update.
 *
 * By default only genuinely-stale feeds are cranked (the production
 * behaviour the consumer scripts rely on). Set `YDELTA_FORCE_CRANK=1` (or
 * pass `--force`) to crank every feed regardless — used to verify the send
 * path end-to-end even when Pyth-DA / Switchboard keep the feeds fresh.
 *
 * Reads:  .local/vaults.json, .local/markets.json, .local/mainnet-banks.json
 * Writes: tx-log.json (crank signatures)
 */
import { PublicKey } from '@solana/web3.js';

import { bankNeedsOracleRefresh } from '../src/oracle.js';
import {
  appendTxLog,
  assertSendAllowed,
  loadConnection,
  loadSigner,
  log,
  readJsonOptional,
} from './_runner.js';
import { readBankOnchain, resolveBankByMint } from './_marginfi.js';
import {
  crankStaleBankOracles,
  oracleSetupLabel,
  type BankOracleCrank,
} from './_oracleCrank.js';
import type { MarketDump, VaultDump } from './_types.js';

async function main(): Promise<void> {
  const conn = loadConnection();
  const cranker = loadSigner();
  const force = process.env.YDELTA_FORCE_CRANK === '1' || process.argv.includes('--force');

  // Any crank broadcasts to chain — gate mainnet the same as sendIxs.
  assertSendAllowed(conn);

  const vaults = readJsonOptional<Record<string, VaultDump>>('vaults.json') ?? {};
  const markets = readJsonOptional<Record<string, MarketDump>>('markets.json') ?? {};

  // Distinct bank set → where it's used (for the report).
  const used = new Map<string, string[]>();
  const note = (bank: string, where: string) => {
    const arr = used.get(bank) ?? [];
    arr.push(where);
    used.set(bank, arr);
  };
  for (const [mint, v] of Object.entries(vaults)) note(v.bank, `vault(${mint.slice(0, 4)}…)`);
  for (const m of Object.values(markets)) {
    note(m.debtBank, `${m.label}.debt`);
    note(m.collateralBank, `${m.label}.collateral`);
  }

  if (used.size === 0) {
    log('[crank-oracle] no banks found in vaults.json / markets.json — nothing to crank');
    return;
  }

  log(`[crank-oracle] ${used.size} distinct bank(s); force=${force}`);
  const cranks: BankOracleCrank[] = [];
  for (const [bankAddr, where] of used) {
    const bank = await readBankOnchain(conn, new PublicKey(bankAddr));
    const entry = resolveBankByMint(bank.mint);
    const stale = await bankNeedsOracleRefresh(conn, bank);
    const staleLabel = stale === null ? 'n/a (undecoded)' : stale ? 'STALE' : 'fresh';
    log(
      `  • ${entry.tokenSymbol.padEnd(8)} bank=${bankAddr.slice(0, 6)}… ` +
        `setup=${oracleSetupLabel(bank.oracleSetup)} maxAge=${bank.oracleMaxAgeSeconds}s ` +
        `oracle=${bank.oracleKeys[0].toBase58().slice(0, 6)}… → ${staleLabel}  [${where.join(', ')}]`,
    );
    cranks.push({
      bank,
      pythFeedIdHex: entry.pythFeedIdHex,
      pythShardId: entry.pythShardId,
    });
  }

  const result = await crankStaleBankOracles(conn, cranker, cranks, { force });

  if (result.entries.length === 0) {
    log(`[crank-oracle] nothing cranked — every feed ${force ? '(force off?)' : 'is fresh'}.`);
    return;
  }
  for (const e of result.entries) {
    log(`[crank-oracle] cranked ${e.provider} oracle=${e.oracle.slice(0, 8)}… sigs=${e.signatures.join(',')}`);
  }
  appendTxLog({
    script: 'crank-oracle',
    signatures: [...result.pythSignatures, ...result.switchboardSignatures],
    oracleCrank: result.entries,
    summary: { force, banksCranked: result.entries.length },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
