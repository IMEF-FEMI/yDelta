/**
 * Oracle-staleness crank helpers.
 *
 * The yDelta marginfi-CPI loaders reject any tx whose bank oracle is past
 * `bank.config.oracle_max_age`. We crank **upfront** in their own
 * transactions before sending the consumer tx — this keeps the consumer
 * tx legacy/small and side-steps the LUT requirements of the Switchboard
 * SDK (and the multi-tx posting flow of the Pyth receiver). Crank txs
 * land in a slot or two, well inside the typical 60–180s `oracle_max_age`
 * window so the freshness check passes when our consumer tx lands.
 *
 * Two providers covered:
 *
 *   - Pyth-Push (`OracleSetup::PythPushOracle`, `StakedWithPythPush`) via
 *     `@pythnetwork/pyth-solana-receiver`. We call `addUpdatePriceFeed`
 *     (NOT `addPostPriceUpdates`) — marginfi reads the canonical
 *     shard-derived PDA, not an ephemeral price-update account.
 *
 *   - Switchboard-Pull (`OracleSetup::SwitchboardPull`) via
 *     `@switchboard-xyz/on-demand`. We construct a `PullFeed` from the
 *     bank's oracleKeys[0] and call `fetchUpdateIx`, then package the
 *     returned ixs into a v0 tx using `sb.asV0Tx`.
 *
 * Stale Pyth without an explicit feed-id is fatal (we can't reverse a
 * shard-PDA back into a feed-id without the catalogue). Stale Switchboard
 * needs only the oracle pubkey — the on-demand SDK fetches the feed
 * config (queue, feedHash, etc.) from the account itself.
 */
import { AnchorProvider, Wallet as NodeWallet } from '@coral-xyz/anchor';
import { PythSolanaReceiver } from '@pythnetwork/pyth-solana-receiver';
import {
  AddressLookupTableAccount,
  Connection,
  Keypair,
  PublicKey,
  TransactionInstruction,
  VersionedTransaction,
} from '@solana/web3.js';

import { Bank, OracleSetup } from '../src/marginfi.js';
import { bankNeedsOracleRefresh } from '../src/oracle.js';
import { hermesUrl, log } from './_runner.js';

/** Input for `crankStaleBankOracles`. One entry per bank we care about. */
export interface BankOracleCrank {
  bank: Bank;
  /** Required when `bank.oracleSetup` is `PythPushOracle` or `StakedWithPythPush`. */
  pythFeedIdHex?: string;
  /** Optional shard override. Default 0 (the shard marginfi uses for every feed today). */
  pythShardId?: number;
}

/**
 * For every stale oracle in `banks`, build + send a one-shot crank
 * transaction. Returns a per-bank result so the operator can record
 * signatures alongside the consumer-tx record.
 *
 * Idempotent in spirit: if every bank is fresh, the function returns
 * `{ entries: [], pythSignatures: [], switchboardSignatures: [] }`.
 */
export async function crankStaleBankOracles(
  conn: Connection,
  payer: Keypair,
  banks: BankOracleCrank[],
  opts: { force?: boolean } = {},
): Promise<CrankResult> {
  const out: CrankResult = {
    entries: [],
    pythSignatures: [],
    switchboardSignatures: [],
  };

  for (const entry of banks) {
    const bank = entry.bank;
    // `force` skips the freshness gate so the crank send path can be
    // exercised even when the feed is already fresh (verification runs).
    // Production callers leave it unset and only crank genuinely-stale feeds.
    if (!opts.force) {
      const stale = await bankNeedsOracleRefresh(conn, bank);
      if (stale !== true) continue; // fresh, or non-cranked setup (we don't crank what we don't decode)
    }
    switch (bank.oracleSetup) {
      case OracleSetup.PythPushOracle:
      case OracleSetup.StakedWithPythPush: {
        if (!entry.pythFeedIdHex) {
          throw new Error(
            `bank with oracle ${bank.oracleKeys[0].toBase58()} is Pyth-Push and stale, ` +
            `but no \`pythFeedIdHex\` was provided. Add it to markets.json under the ` +
            `relevant market (or the script's input file).`,
          );
        }
        const sigs = await crankPythPush(conn, payer, entry.pythFeedIdHex, entry.pythShardId ?? 0);
        out.pythSignatures.push(...sigs);
        out.entries.push({
          oracle: bank.oracleKeys[0].toBase58(),
          provider: 'pyth-push',
          signatures: sigs,
        });
        break;
      }
      case OracleSetup.SwitchboardPull: {
        const sig = await crankSwitchboardPull(conn, payer, bank.oracleKeys[0]);
        out.switchboardSignatures.push(sig);
        out.entries.push({
          oracle: bank.oracleKeys[0].toBase58(),
          provider: 'switchboard-pull',
          signatures: [sig],
        });
        break;
      }
      default:
        // Other setups (PythLegacy, SwitchboardV2, Kamino/Drift/Solend/JupLend
        // wrappers, Fixed*) aren't decoded by our freshness probe so they
        // never report stale — we never get here.
        break;
    }
  }
  return out;
}

export interface CrankResult {
  entries: Array<{
    oracle: string;
    provider: 'pyth-push' | 'switchboard-pull';
    signatures: string[];
  }>;
  pythSignatures: string[];
  switchboardSignatures: string[];
}

/* ── Pyth-Push ──────────────────────────────────────────────── */

async function crankPythPush(
  conn: Connection,
  payer: Keypair,
  pythFeedIdHex: string,
  shardId: number,
): Promise<string[]> {
  const wallet = new NodeWallet(payer);
  const receiver = new PythSolanaReceiver({ connection: conn, wallet });
  const vaa = await fetchHermesUpdate(pythFeedIdHex);
  // `closeUpdateAccounts: true` appends a close-encoded-VAA ix to the
  // tail of the tx batch so the payer recovers the ~few-mSOL rent the
  // SDK allocates for the wormhole VAA. We don't accumulate stale VAA
  // accounts that way.
  const builder = receiver.newTransactionBuilder({ closeUpdateAccounts: true });
  // `addUpdatePriceFeed` targets the canonical shard-PDA marginfi reads
  // (`getPriceFeedAccountForProgram(shardId, feedId)`). Do NOT swap to
  // `addPostPriceUpdates` here — that posts to an ephemeral account
  // marginfi has no knowledge of, and the on-chain freshness check
  // would still fail.
  await builder.addUpdatePriceFeed([vaa], shardId);
  const microLamports = priorityFeeMicroLamports();
  const versioned = await builder.buildVersionedTransactions({
    computeUnitPriceMicroLamports: microLamports,
  });
  log(
    `[oracle/pyth] cranking feed ${pythFeedIdHex.slice(0, 8)}… ` +
    `(shard=${shardId}, ${versioned.length} tx, priority=${microLamports} µL)`,
  );
  const provider = new AnchorProvider(conn, wallet, { commitment: 'confirmed' });
  const out: string[] = [];
  for (const v of versioned) {
    const tx = v.tx;
    if (v.signers && v.signers.length > 0) tx.sign(v.signers);
    const sig = await provider.sendAndConfirm(tx);
    out.push(sig);
  }
  return out;
}

/**
 * Per-CU priority fee for the oracle crank txs, configurable via
 * `YDELTA_PRIORITY_FEE_MICROLAMPORTS`. Mainnet default is high enough to
 * land during normal congestion (~50k µL is the marginfi-script default);
 * operators on a private RPC can drop it to 0 to skip priority fees.
 */
function priorityFeeMicroLamports(): number {
  const raw = process.env.YDELTA_PRIORITY_FEE_MICROLAMPORTS;
  if (raw === undefined || raw === '') return 50_000;
  const n = Number(raw);
  if (!Number.isFinite(n) || n < 0) {
    throw new Error(
      `YDELTA_PRIORITY_FEE_MICROLAMPORTS must be a non-negative number, got "${raw}"`,
    );
  }
  return Math.floor(n);
}

async function fetchHermesUpdate(feedIdHex: string): Promise<string> {
  const id = feedIdHex.startsWith('0x') ? feedIdHex.slice(2) : feedIdHex;
  const url = `${hermesUrl()}/v2/updates/price/latest?ids%5B%5D=${id}&encoding=base64`;
  const res = await fetch(url);
  if (!res.ok) {
    throw new Error(`hermes latest-update fetch failed for ${id}: ${res.status} ${res.statusText}`);
  }
  const body = (await res.json()) as { binary: { encoding: string; data: string[] } };
  const data = body.binary?.data?.[0];
  if (!data) {
    throw new Error(`hermes latest-update for ${id} returned empty body`);
  }
  return data;
}

/* ── Switchboard-Pull ──────────────────────────────────────── */

async function crankSwitchboardPull(
  conn: Connection,
  payer: Keypair,
  pullFeedPubkey: PublicKey,
): Promise<string> {
  // Dynamic import keeps Switchboard's transitive Anchor-31 dependency
  // out of the hot path for scripts that never crank Switchboard.
  const sb = await import('@switchboard-xyz/on-demand');
  const { CrossbarClient } = await import('@switchboard-xyz/common');
  const wallet = new NodeWallet(payer);
  const provider = new AnchorProvider(conn, wallet, { commitment: 'confirmed' });
  const program = await sb.AnchorUtils.loadProgramFromConnection(conn, wallet);
  const pullFeed = new sb.PullFeed(program, pullFeedPubkey);
  const queue = await sb.Queue.loadDefault(program);
  // Private-endpoint overrides so a self-hosted Crossbar (e.g. on Railway)
  // can replace the rate-limited public infra:
  //   YDELTA_SWITCHBOARD_CROSSBAR — base URL of your own Crossbar instance
  //   YDELTA_SWITCHBOARD_GATEWAY  — pin one oracle gateway (skip discovery)
  const _cbUrl = process.env.YDELTA_SWITCHBOARD_CROSSBAR?.trim();
  const crossbar = _cbUrl ? new CrossbarClient(_cbUrl, true) : CrossbarClient.default();
  const _gwPin = process.env.YDELTA_SWITCHBOARD_GATEWAY?.trim();
  const gateway =
    _gwPin && _gwPin.length > 0
      ? _gwPin
      : (await queue.fetchGatewayFromCrossbar(crossbar)).gatewayUrl;
  log(`[oracle/swb] cranking pull feed ${pullFeedPubkey.toBase58()}`);

  const [ixs, _responses, _success, luts] = await pullFeed.fetchUpdateIx({
    gateway,
    numSignatures: 1,
  });
  if (!ixs || ixs.length === 0) {
    throw new Error(`switchboard fetchUpdateIx returned no instructions for ${pullFeedPubkey.toBase58()}`);
  }
  // `computeUnitLimitMultiple: 2` mirrors the marginfi mainnet crank
  // script's setting — leaves headroom over the SDK's measured CU
  // estimate so the tx doesn't bounce off a too-tight limit. Priority
  // fee mirrors the Pyth knob so operators tune both in one place.
  const tx: VersionedTransaction = await sb.asV0Tx({
    connection: conn,
    ixs,
    signers: [payer],
    lookupTables: luts as AddressLookupTableAccount[],
    payer: payer.publicKey,
    computeUnitLimitMultiple: 2,
    computeUnitPrice: priorityFeeMicroLamports(),
  });
  const sig = await provider.sendAndConfirm(tx);
  // `_responses` / `_success` are surfaced by the SDK for diagnostics
  // (oracle signatures + per-feed success). We don't act on them here —
  // a failed signature would have made `ixs` empty.
  void _responses;
  void _success;
  void TransactionInstruction; // exported types stay referenced for downstream importers
  return sig;
}

/* ── Switchboard bundle path ───────────────────────────────── */

/**
 * Build (do NOT send) the Switchboard-pull update ixs for `oracleKeys`,
 * plus the lookup tables they reference. Caller prepends the ixs to its
 * consumer tx so the freshly-posted price and the consuming ix execute
 * atomically in one v0 transaction. Switchboard On-Demand feeds are
 * pull-based and can't be cheaply probed for staleness, so we always
 * rebuild — the cost is one bundled update per consuming tx.
 */
export async function buildSwitchboardUpdateIxs(
  conn: Connection,
  payer: Keypair,
  oracleKeys: PublicKey[],
): Promise<{ ixs: TransactionInstruction[]; luts: AddressLookupTableAccount[] }> {
  if (oracleKeys.length === 0) return { ixs: [], luts: [] };
  const sb = await import('@switchboard-xyz/on-demand');
  const { CrossbarClient } = await import('@switchboard-xyz/common');
  const wallet = new NodeWallet(payer);
  const program = await sb.AnchorUtils.loadProgramFromConnection(conn, wallet);
  const queue = await sb.Queue.loadDefault(program);
  // Private-endpoint overrides so a self-hosted Crossbar (e.g. on Railway)
  // can replace the rate-limited public infra:
  //   YDELTA_SWITCHBOARD_CROSSBAR — base URL of your own Crossbar instance
  //   YDELTA_SWITCHBOARD_GATEWAY  — pin one oracle gateway (skip discovery)
  const _cbUrl = process.env.YDELTA_SWITCHBOARD_CROSSBAR?.trim();
  const crossbar = _cbUrl ? new CrossbarClient(_cbUrl, true) : CrossbarClient.default();
  const _gwPin = process.env.YDELTA_SWITCHBOARD_GATEWAY?.trim();
  const gateway =
    _gwPin && _gwPin.length > 0
      ? _gwPin
      : (await queue.fetchGatewayFromCrossbar(crossbar)).gatewayUrl;

  const ixs: TransactionInstruction[] = [];
  const luts: AddressLookupTableAccount[] = [];
  for (const oracle of oracleKeys) {
    const pullFeed = new sb.PullFeed(program, oracle);
    const [feedIxs, _responses, _success, feedLuts] = await pullFeed.fetchUpdateIx({
      gateway,
      numSignatures: 1,
    });
    if (!feedIxs || feedIxs.length === 0) {
      throw new Error(`switchboard fetchUpdateIx returned no instructions for ${oracle.toBase58()}`);
    }
    void _responses;
    void _success;
    ixs.push(...feedIxs);
    luts.push(...((feedLuts ?? []) as AddressLookupTableAccount[]));
    log(`[oracle/swb] built update ix for pull feed ${oracle.toBase58().slice(0, 8)}…`);
  }
  return { ixs, luts };
}

/* ── Misc ──────────────────────────────────────────────────── */

export function oracleSetupLabel(setup: OracleSetup): string {
  return OracleSetup[setup] ?? `Unknown(${setup})`;
}
