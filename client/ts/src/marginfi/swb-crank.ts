/**
 * Switchboard On-Demand crank helpers — UI/browser side.
 *
 * Pattern matches marginfi-client-v2's `createUpdateFeedIx` /
 * `makeUpdateFeedIx` (see `node_modules/@mrgnlabs/marginfi-client-v2/
 * src/services/account/account.service.ts` and `models/account/
 * wrapper.ts`). Same call into `PullFeed.fetchUpdateManyIx` from
 * `@switchboard-xyz/on-demand`; same two-tx pattern eva01 uses on the
 * server side (`references/eva01/src/utils/swb_cranker.rs`).
 *
 * yDelta needs this because the matching engine reads `BankConfig`
 * oracle freshness via `MarginfiV18Adapter.oracle_price`, and the
 * collateral side of every (`debt`,`collateral`) pair on the SOL/USDC
 * market currently uses a Switchboard-Pull feed (`oracle_setup = 4`).
 * If no keeper has cranked the feed within the bank's `oracle_max_age`
 * window, every `PlaceOrder` / `LiquidateLoan` / `ConvertP2PoolToFixed`
 * call hits `AdapterError::OracleStale (custom 102)`.
 *
 * Use {@link makeSwbCrankIxsForBanks} when you have a set of bank
 * pubkeys (the common UI case — you already know which banks the
 * downstream ix touches). It loads each bank, keeps only the SWB-Pull
 * banks, and builds the crank ixs + LUTs.
 *
 * Use {@link makeSwbCrankIxs} when you already have raw oracle pubkeys.
 *
 * Both return `{ instructions, luts }`. Callers compose into a v0 tx;
 * pass `luts` to `TransactionMessage.compileToV0Message(luts)`.
 *
 * **Submission shape**: the marginfi-client-v2 default is two
 * sequential v0 txs — `[crankTx, actionTx]` — because the combined
 * footprint can exceed Solana's 1232-byte tx limit. Single-tx
 * (crank + action) is possible when both fit; prefer it for
 * atomicity, fall back to two txs otherwise.
 */

import {
  AddressLookupTableAccount,
  Connection,
  PublicKey,
  TransactionInstruction,
} from '@solana/web3.js';
import { AnchorUtils, PullFeed } from '@switchboard-xyz/on-demand';
import { CrossbarClient } from '@switchboard-xyz/common';

import { loadBankSnapshot } from './client';

/**
 * Marginfi's sentinel "no oracle" feed pubkey — banks with this in
 * `oracle_keys[0]` should be skipped entirely. Mirrored from
 * marginfi-client-v2's hard-coded filter.
 */
const SWB_ZERO_FEED = new PublicKey('DMhGWtLAKE5d56WdyHQxqeFncwUeqMEnuC2RvvZfbuur');

/**
 * Marginfi `OracleSetup::SwitchboardPull` tag value. Mirrored from the
 * program's `marginfi-mocks::state::OracleSetup` enum (variant 4).
 * Banks with other setups don't need (or can't use) Switchboard
 * cranking.
 */
export const ORACLE_SETUP_SWITCHBOARD_PULL = 4;

export type MakeSwbCrankIxsResult = {
  /** Crank ixs to prepend to the action tx, or send as their own v0 tx. */
  instructions: TransactionInstruction[];
  /** Address Lookup Tables required by the crank ixs. */
  luts: AddressLookupTableAccount[];
};

export type MakeSwbCrankIxsArgs = {
  connection: Connection;
  payer: PublicKey;
  /** Switchboard `PullFeedAccountData` pubkeys (`bank.config.oracleKeys[0]`). */
  oracles: PublicKey[];
  /** Number of oracle signatures Crossbar should aggregate. Default 1
   *  matches marginfi-client-v2 and eva01. */
  numSignatures?: number;
  /** Override the default `https://crossbar.switchboard.xyz` endpoint.
   *  Use `NEXT_PUBLIC_SWITCHBOARD_CROSSSBAR_API`-style configs (or
   *  self-hosted gateways) by constructing a CrossbarClient against
   *  your gateway URL and passing it here. */
  crossbarClient?: CrossbarClient;
};

/**
 * Low-level helper: build Switchboard "fetch update" ixs for a set of
 * oracle pubkeys. Returns an empty result when `oracles` is empty (or
 * every entry is the sentinel zero-feed).
 *
 * The caller compiles `instructions` + `luts` into a v0 transaction.
 */
export async function makeSwbCrankIxs(args: MakeSwbCrankIxsArgs): Promise<MakeSwbCrankIxsResult> {
  const filteredOracles = args.oracles.filter((o) => !o.equals(SWB_ZERO_FEED));
  if (filteredOracles.length === 0) {
    return { instructions: [], luts: [] };
  }

  const swbProgram = await AnchorUtils.loadProgramFromConnection(args.connection);
  const pullFeeds: PullFeed[] = filteredOracles.map((pk) => new PullFeed(swbProgram, pk));
  const crossbar = args.crossbarClient;
  const gateway = await pullFeeds[0].fetchGatewayUrl(crossbar);

  const [instructions, luts] = await PullFeed.fetchUpdateManyIx(swbProgram, {
    feeds: pullFeeds,
    gateway,
    numSignatures: args.numSignatures ?? 1,
    payer: args.payer,
    crossbarClient: crossbar,
  });

  return { instructions, luts };
}

export type MakeSwbCrankIxsForBanksArgs = {
  connection: Connection;
  payer: PublicKey;
  /** Bank pubkeys the downstream ix will touch. Non-SWB banks are
   *  silently skipped. */
  banks: PublicKey[];
  numSignatures?: number;
  /** Self-hosted Crossbar override — see {@link MakeSwbCrankIxsArgs.crossbarClient}. */
  crossbarClient?: CrossbarClient;
};

export type MakeSwbCrankIxsForBanksResult = MakeSwbCrankIxsResult & {
  /** Bank pubkeys that were SWB-Pull (subset of input `banks`). Empty
   *  when the helper found nothing to crank. */
  swbBanks: PublicKey[];
  /** Resolved oracle pubkeys, one per `swbBanks` entry (same order). */
  swbOracles: PublicKey[];
};

/**
 * UI-side helper: load each `bank`, filter to those whose primary
 * oracle is `OracleSetup::SwitchboardPull`, and build the crank ixs.
 *
 * Equivalent of marginfi-client-v2's
 * `MarginfiAccountWrapper.makeUpdateFeedIx(banks)` — the
 * `makeBorrowTx` etc. wrappers call this before the action ix path.
 */
export async function makeSwbCrankIxsForBanks(
  args: MakeSwbCrankIxsForBanksArgs,
): Promise<MakeSwbCrankIxsForBanksResult> {
  const swbBanks: PublicKey[] = [];
  const swbOracles: PublicKey[] = [];
  for (const bankPk of args.banks) {
    const snapshot = await loadBankSnapshot(args.connection, bankPk);
    if (snapshot.oracleSetup !== ORACLE_SETUP_SWITCHBOARD_PULL) continue;
    const oracle = snapshot.oracleKeys[0];
    if (!oracle || oracle.equals(PublicKey.default) || oracle.equals(SWB_ZERO_FEED)) continue;
    swbBanks.push(bankPk);
    swbOracles.push(oracle);
  }

  if (swbOracles.length === 0) {
    return { instructions: [], luts: [], swbBanks: [], swbOracles: [] };
  }

  const { instructions, luts } = await makeSwbCrankIxs({
    connection: args.connection,
    payer: args.payer,
    oracles: swbOracles,
    numSignatures: args.numSignatures,
    crossbarClient: args.crossbarClient,
  });

  return { instructions, luts, swbBanks, swbOracles };
}
