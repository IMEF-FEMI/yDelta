#!/usr/bin/env -S npx ts-node
/* eslint-disable no-console */
/**
 * Place a primary Limit Ask on the SOL/USDC orderbook using the new TS
 * SDK helpers — same flow the `crankers/src/bin/place_order.rs` binary
 * runs, but from the UI/SDK side so it exercises:
 *
 *   - `placeAskIxs` (helpers/compose) → the action ix
 *   - `makeSwbCrankIxsForBanks` (marginfi/swb-crank) → the SWB pre-flight
 *   - `buildCrankAndActionV0Txs` (helpers/compose) → packages both into
 *     two v0 txs the wallet can sign sequentially, matching
 *     marginfi-client-v2's `makeBorrowTx` shape
 *
 * Usage (from repo root):
 *
 *   yarn ts-node scripts/setup/place-ask.ts \
 *     --cluster mainnet \
 *     --market <MARKET_PUBKEY> \
 *     [--payer ~/.config/solana/id.json] \
 *     [--principal 10_000_000]   # debt-mint atoms ($10 USDC at 6dp)
 *     [--rate-bps 720]           # 7.20% APY
 *     [--term-secs 1209600]      # 14d
 *     [--last-valid-ts 0]        # 0 = no expiry
 *     [--flags 0] [--confirm-mainnet] [--dry-run]
 *
 * Pre-requisites (mirrors the Rust binary):
 *   - Payer has USDC deposited on their `ClaimedSeat` for `--market`
 *     (run `Deposit` first if not). Asks encumber withdrawable USDC
 *     shares — the tx reverts with `InsufficientWithdrawableBalance`
 *     (custom 14) when the seat is empty.
 *   - Payer has enough SOL for tx fees + ATA rent if their USDC ATA
 *     doesn't exist yet.
 */
import * as fs from 'fs';
import { Connection, PublicKey, VersionedTransaction } from '@solana/web3.js';
import {
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  getAssociatedTokenAddressSync,
} from '@solana/spl-token';
import BN from 'bn.js';

import {
  assertNotDevnet,
  confirmMainnetIfNeeded,
  defaultCluster,
  defaultPayerPath,
  fmtKey,
  header,
  loadDotEnv,
  loadKeypair,
  makeConnection,
  optionalArg,
  parseArgs,
  requireArg,
} from './common';
import { Market } from '../../client/ts/src/market';
import { loadBankSnapshot } from '../../client/ts/src/marginfi/client';
import {
  buildCrankAndActionV0Txs,
  placeAskIxs,
} from '../../client/ts/src/helpers/compose';

/** Marginfi v2 mainnet program id — pinned in the SDK already, but the
 *  Ask ix-builder takes the program key explicitly so we surface it
 *  here too. */
const MARGINFI_PROGRAM_ID = new PublicKey('MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA');

async function main() {
  loadDotEnv();
  const args = parseArgs(process.argv.slice(2));
  const cluster = optionalArg(args, 'cluster') ?? defaultCluster() ?? 'mainnet';
  const marketKey = new PublicKey(requireArg(args, 'market'));
  const payer = loadKeypair(optionalArg(args, 'payer') ?? defaultPayerPath());

  const principalAtoms = parseAtoms(optionalArg(args, 'principal') ?? '10000000');
  const rateBps = parseUint(optionalArg(args, 'rate-bps') ?? '720', 'rate-bps');
  const termSecs = parseUint(optionalArg(args, 'term-secs') ?? String(14 * 24 * 60 * 60), 'term-secs');
  const lastValidTs = new BN(optionalArg(args, 'last-valid-ts') ?? '0');
  const flags = parseUint(optionalArg(args, 'flags') ?? '0', 'flags');
  const dryRun = args['dry-run'] === true;

  const connection = makeConnection(cluster);
  assertNotDevnet(cluster, connection.rpcEndpoint);
  await confirmMainnetIfNeeded(connection, cluster, args);

  header('Loading market');
  const market = await Market.loadFromAddress({ connection, address: marketKey });
  const debtMint = market.debtMint();
  const collateralMint = market.collateralMint();
  const debtBankPk = market.debtBank();
  const collateralBankPk = market.collateralBank();
  console.log(`market           = ${fmtKey(marketKey)}`);
  console.log(`  debt mint      = ${fmtKey(debtMint)}`);
  console.log(`  collateral mint= ${fmtKey(collateralMint)}`);
  console.log(`  debt bank      = ${fmtKey(debtBankPk)}`);
  console.log(`  collateral bank= ${fmtKey(collateralBankPk)}`);

  header('Loading marginfi banks');
  const [debtBank, collateralBank] = await Promise.all([
    loadBankSnapshot(connection, debtBankPk),
    loadBankSnapshot(connection, collateralBankPk),
  ]);

  const debtOracles = debtBank.oracleKeys;
  const collateralOracles = collateralBank.oracleKeys;

  // Derive debt bank's `liquidity_vault_authority` PDA. The seeds are
  // `["liquidity_vault_auth", bank_pubkey]` against the marginfi
  // program; the bump is stored on the bank itself
  // (`liquidityVaultAuthorityBump`) so we can `createProgramAddressSync`
  // without re-deriving.
  const debtBankLva = PublicKey.createProgramAddressSync(
    [
      Buffer.from('liquidity_vault_auth'),
      debtBankPk.toBuffer(),
      Buffer.from([debtBank.liquidityVaultAuthorityBump]),
    ],
    MARGINFI_PROGRAM_ID,
  );

  // SPL Token program — yDelta's create_market validates the bank's
  // mint owner matches the token program in accounts. Mainnet USDC is
  // legacy token; wSOL is also legacy. If a market ever switches to a
  // Token-2022 mint, swap based on the mint's account owner.
  const tokenProgram = TOKEN_PROGRAM_ID;
  const _t22 = TOKEN_2022_PROGRAM_ID; // silence import lint

  const borrowerDebtToken = getAssociatedTokenAddressSync(
    debtMint,
    payer.publicKey,
    false,
    tokenProgram,
  );

  header('Building Ask ix');
  console.log(`signer           = ${fmtKey(payer.publicKey)}`);
  console.log(`  debt ATA       = ${fmtKey(borrowerDebtToken)}`);
  console.log(`  principal      = ${principalAtoms.toString()} debt-atoms`);
  console.log(`  rate           = ${rateBps} bps`);
  console.log(`  term           = ${termSecs}s (${(termSecs / 86400).toFixed(2)}d)`);

  const actionIxs = placeAskIxs(
    {
      market,
      payer: payer.publicKey,
      marginfiGroup: market.marginfiGroup(),
      debtBank: debtBankPk,
      collateralBank: collateralBankPk,
      debtOracles,
      collateralOracles,
      debtLiquidityVault: debtBank.liquidityVault,
      debtBankLiquidityVaultAuthority: debtBankLva,
      borrowerDebtToken,
      tokenProgram,
      marginfiProgram: MARGINFI_PROGRAM_ID,
    },
    {
      rateBps,
      termSeconds: termSecs,
      principalAtoms,
      lastValidUnixTs: lastValidTs,
      flags,
    },
  );

  header('Building crank + action v0 txs');
  const { crankTx, crankLuts, actionTx, swbBanks } = await buildCrankAndActionV0Txs({
    connection,
    payer: payer.publicKey,
    banks: [debtBankPk, collateralBankPk],
    actionIxs,
  });
  if (crankTx) {
    console.log(`crank tx required — ${swbBanks.length} SWB-Pull bank(s):`);
    for (const b of swbBanks) console.log(`  - ${fmtKey(b)}`);
  } else {
    console.log('no SWB-Pull banks touched → no crank tx needed');
  }

  if (dryRun) {
    console.log('\n--dry-run: built both txs, not signing or submitting');
    return;
  }

  if (crankTx) {
    header('Submitting crank tx');
    crankTx.sign([payer]);
    const crankSig = await connection.sendRawTransaction(crankTx.serialize(), {
      skipPreflight: false,
    });
    console.log(`crank sig        = ${crankSig}`);
    await connection.confirmTransaction(
      {
        signature: crankSig,
        ...(await connection.getLatestBlockhash('confirmed')),
      },
      'confirmed',
    );
    console.log('crank confirmed');

    // The action tx's blockhash was set inside buildCrankAndActionV0Txs.
    // Marginfi-client-v2's pattern is fine with the (small) chance that
    // the blockhash expires between crank confirm and action send;
    // re-compile with a fresh blockhash here to be safe on slow RPCs.
    void crankLuts; // already attached to crankTx
    actionTx.message.recentBlockhash = (await connection.getLatestBlockhash('confirmed')).blockhash;
    // V0 messages re-sign cleanly when the blockhash field is mutated
    // pre-signing; we haven't signed yet so this is a no-op safety.
  }

  header('Submitting action tx (PlaceOrder Ask)');
  actionTx.sign([payer]);
  const actionSig = await connection.sendRawTransaction(actionTx.serialize(), {
    skipPreflight: false,
  });
  console.log(`action sig       = ${actionSig}`);
  await connection.confirmTransaction(
    {
      signature: actionSig,
      ...(await connection.getLatestBlockhash('confirmed')),
    },
    'confirmed',
  );
  console.log('action confirmed');
  console.log(`\nDone. tx: ${actionSig}`);
}

function parseAtoms(s: string): BN {
  // Tolerate `10_000_000` / `10,000,000` / `10000000`.
  return new BN(s.replace(/[_,]/g, ''));
}

function parseUint(s: string, label: string): number {
  const cleaned = s.replace(/[_,]/g, '');
  const n = Number(cleaned);
  if (!Number.isFinite(n) || n < 0 || !Number.isInteger(n)) {
    throw new Error(`--${label}: expected non-negative integer, got ${s}`);
  }
  return n;
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
