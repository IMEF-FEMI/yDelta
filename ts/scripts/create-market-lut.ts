/**
 * create-market-lut.ts — deploy a per-market Address Lookup Table.
 *
 * The market health-checked ixs (seat withdraw, place-bid, settle, liquidate)
 * carry ~21 accounts. To bundle a Switchboard oracle update into the SAME
 * v0 transaction (so the fresh price and the consuming ix execute atomically)
 * the consumer's accounts must compress via a LUT to fit the 1232-byte tx
 * limit. This script creates one LUT per market, populated with every static
 * account those ixs reference, and records its address in markets.json.
 *
 * Idempotent: skips a market whose `lookupTable` is already recorded and live
 * on-chain.
 *
 * Reads:  .local/markets.json
 * Writes: .local/markets.json (adds `lookupTable` per market) + tx-log.json
 */
import {
  AddressLookupTableProgram,
  PublicKey,
  SystemProgram,
} from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  borrowerIntegrationAccountPda,
  globalConfigPda,
  lenderIntegrationAccountPda,
  marketSignerPda,
  marketTokenVaultPda,
} from '../src/pdas.js';
import {
  appendTxLog,
  loadConnection,
  loadSigner,
  log,
  readJson,
  sendIxs,
  writeJson,
} from './_runner.js';
import { MARGINFI_PROGRAM_ID } from './_marginfi.js';
import type { MarketDump } from './_types.js';

/** Static accounts every health-checked ix on this market references. */
function marketLutAddresses(m: MarketDump): PublicKey[] {
  const market = new PublicKey(m.market);
  const debtMint = new PublicKey(m.debtMint);
  const collateralMint = new PublicKey(m.collateralMint);
  const addrs = [
    globalConfigPda()[0],
    market,
    marketTokenVaultPda(market, debtMint)[0],
    marketTokenVaultPda(market, collateralMint)[0],
    debtMint,
    collateralMint,
    new PublicKey(m.marginfiGroup),
    lenderIntegrationAccountPda(market)[0],
    borrowerIntegrationAccountPda(market)[0],
    new PublicKey(m.debtBank),
    new PublicKey(m.collateralBank),
    new PublicKey(m.debtLiquidityVault),
    new PublicKey(m.collateralLiquidityVault),
    new PublicKey(m.debtBankLiquidityVaultAuthority),
    new PublicKey(m.collateralBankLiquidityVaultAuthority),
    ...m.debtOracles.map((s) => new PublicKey(s)),
    ...m.collateralOracles.map((s) => new PublicKey(s)),
    marketSignerPda(market)[0],
    MARGINFI_PROGRAM_ID,
    TOKEN_PROGRAM_ID,
    SystemProgram.programId,
  ];
  // Dedupe (an oracle/program could coincide) — order is irrelevant for a LUT.
  const seen = new Set<string>();
  return addrs.filter((p) => {
    const k = p.toBase58();
    if (seen.has(k)) return false;
    seen.add(k);
    return true;
  });
}

async function main(): Promise<void> {
  const conn = loadConnection();
  const signer = loadSigner();
  const markets = readJson<Record<string, MarketDump>>('markets.json');

  for (const [label, m] of Object.entries(markets)) {
    if (m.lookupTable) {
      const existing = await conn.getAddressLookupTable(new PublicKey(m.lookupTable));
      if (existing.value) {
        log(`[create-market-lut] ${label}: LUT ${m.lookupTable} already live (${existing.value.state.addresses.length} addrs); skipping`);
        continue;
      }
      log(`[create-market-lut] ${label}: recorded LUT ${m.lookupTable} not found on-chain; recreating`);
    }

    const addresses = marketLutAddresses(m);
    const recentSlot = await conn.getSlot('finalized');
    const [createIx, lutAddress] = AddressLookupTableProgram.createLookupTable({
      authority: signer.publicKey,
      payer: signer.publicKey,
      recentSlot,
    });
    const extendIx = AddressLookupTableProgram.extendLookupTable({
      payer: signer.publicKey,
      authority: signer.publicKey,
      lookupTable: lutAddress,
      addresses,
    });

    log(`[create-market-lut] ${label}: creating LUT ${lutAddress.toBase58()} with ${addresses.length} addrs`);
    const sig = await sendIxs(conn, signer, [createIx, extendIx]);
    log(`[create-market-lut] ${label}: signature = ${sig}`);

    markets[label] = { ...m, lookupTable: lutAddress.toBase58() };
    writeJson('markets.json', markets);
    appendTxLog({
      script: 'create-market-lut',
      signatures: [sig],
      summary: { label, lookupTable: lutAddress.toBase58(), addresses: addresses.length },
    });
  }

  log('[create-market-lut] done. Note: a LUT is usable ~1 slot after creation.');
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
