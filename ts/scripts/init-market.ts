/**
 * init-market.ts — `SystemProgram.createAccount` + `CreateMarket` (tag 0)
 * for every entry in `.local/markets-input.json`.
 *
 * Each market's debt/collateral mint, marginfi bank pubkeys, and marginfi
 * group come explicitly from `.local/markets-input.json`. The bank oracles
 * are read and verified on-chain from the bank accounts — there is no
 * static-registry lookup.
 *
 * Reads:
 *   .local/markets-input.json {
 *     markets: [{
 *       label: string,                      // e.g. "USDC/SOL"
 *       debtMint: string,
 *       collateralMint: string,
 *       debtBank: string,                   // marginfi bank pubkeys
 *       collateralBank: string,
 *       marginfiGroup: string,
 *       debtPythFeedIdHex?: string,         // for Pyth-Push debt banks
 *       params?: { ...CreateMarketParams }
 *     }]
 *   }
 *   .local/global-config.json
 * Writes:
 *   .local/markets.json {
 *     [label]: {
 *       market, debtMint, collateralMint, debtBank, collateralBank,
 *       marginfiGroup, marketSecretKeyBase58, signature?
 *     }
 *   }
 *
 * `markets.json` is the full per-market kit downstream scripts read.
 */
import {
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
} from '@solana/web3.js';
import bs58 from 'bs58';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import { createMarketInstruction } from '../src/instructions/index.js';
import { MARKET_FIXED_SIZE } from '../src/accounts/market.js';
import { YDELTA_PROGRAM_ID } from '../src/constants.js';
import {
  appendTxLog,
  assertSendAllowed,
  loadConnection,
  loadGlobalConfig,
  loadSigner,
  log,
  readJson,
  readJsonOptional,
  sendIxs,
  writeJson,
} from './_runner.js';
import {
  bankLiquidityVaultAuthority,
  bankOracleAccounts,
  MARGINFI_PROGRAM_ID,
  readBankOnchain,
} from './_marginfi.js';
import type { CreateMarketParams } from '../src/instructions/createMarket.js';
import type { Bank } from '../src/marginfi.js';

interface MarketInput {
  label: string;
  debtMint: string;
  collateralMint: string;
  debtBank: string;
  collateralBank: string;
  marginfiGroup: string;
  marginfiProgram?: string;
  /** Optional: hex feed id (no 0x) for the debt bank's Pyth-Push oracle. */
  debtPythFeedIdHex?: string;
  /** Optional: pyth shard id for the debt bank's oracle, default 0. */
  debtPythShardId?: number;
  collateralPythFeedIdHex?: string;
  collateralPythShardId?: number;
  params?: CreateMarketParams;
}

interface MarketsInput {
  markets: MarketInput[];
}

interface MarketDump {
  label: string;
  market: string;
  marketSecretKeyBase58: string;
  debtMint: string;
  collateralMint: string;
  debtBank: string;
  debtLiquidityVault: string;
  debtBankLiquidityVaultAuthority: string;
  debtOracles: string[];
  debtPythFeedIdHex?: string;
  debtPythShardId?: number;
  collateralBank: string;
  collateralLiquidityVault: string;
  collateralBankLiquidityVaultAuthority: string;
  collateralOracles: string[];
  collateralPythFeedIdHex?: string;
  collateralPythShardId?: number;
  marginfiGroup: string;
  signature?: string;
}

async function main(): Promise<void> {
  loadGlobalConfig();
  const input = readJson<MarketsInput>('markets-input.json');
  const conn = loadConnection();
  const signer = loadSigner();

  const all = readJsonOptional<Record<string, MarketDump>>('markets.json') ?? {};

  for (const m of input.markets) {
    if (all[m.label]) {
      log(`[init-market] ${m.label} already exists in markets.json; skipping`);
      continue;
    }
    log(`\n[init-market] === ${m.label} ===`);
    const debtMint = new PublicKey(m.debtMint);
    const collateralMint = new PublicKey(m.collateralMint);
    const marginfiGroup = new PublicKey(m.marginfiGroup);
    const marginfiProgram = new PublicKey(m.marginfiProgram ?? MARGINFI_PROGRAM_ID);
    if (!marginfiProgram.equals(MARGINFI_PROGRAM_ID)) {
      throw new Error(
        `markets-input.json[${m.label}] marginfiProgram ${marginfiProgram.toBase58()} ≠ pinned ` +
        `v0.1.8 program id ${MARGINFI_PROGRAM_ID.toBase58()}`,
      );
    }

    const debt = await readAndVerifyBank(conn, m.debtBank, debtMint, marginfiGroup, `${m.label}.debt`);
    const collateral = await readAndVerifyBank(conn, m.collateralBank, collateralMint, marginfiGroup, `${m.label}.collateral`);

    log(`[init-market] debt       bank=${debt.address.toBase58()} oracle=${debt.bank.oracleKeys[0].toBase58()}`);
    log(`[init-market] collateral bank=${collateral.address.toBase58()} oracle=${collateral.bank.oracleKeys[0].toBase58()}`);

    // `CreateMarket` doesn't read oracles, so no crank here. Subsequent
    // ixs (deposit/withdraw/place-order/settle/liquidate) will crank
    // on-demand via `_oracleCrank.crankStaleBankOracles`.

    const marketKp = Keypair.generate();
    log(`[init-market] new market pubkey = ${marketKp.publicKey.toBase58()}`);
    const rent = await conn.getMinimumBalanceForRentExemption(MARKET_FIXED_SIZE);
    const allocateIx = SystemProgram.createAccount({
      fromPubkey: signer.publicKey,
      newAccountPubkey: marketKp.publicKey,
      space: MARKET_FIXED_SIZE,
      lamports: rent,
      programId: YDELTA_PROGRAM_ID,
    });

    // The marginfi accounts inside `create_market` are funded by the
    // signer via CPI, so the signer must hold enough lamports for the
    // market account rent + two marginfi-account rents (~0.05 SOL).
    const allocateSig = await sendAllocate(conn, signer, marketKp, allocateIx);
    log(`[init-market] allocate signature = ${allocateSig}`);

    const createIx = createMarketInstruction({
      marketCreator: signer.publicKey,
      market: marketKp.publicKey,
      debtMint,
      collateralMint,
      marginfiGroup,
      debtBank: debt.address,
      collateralBank: collateral.address,
      marginfiProgram,
      params: m.params,
    });
    const sig = await sendIxs(conn, signer, [createIx]);
    log(`[init-market] create_market signature = ${sig}`);

    all[m.label] = {
      label: m.label,
      market: marketKp.publicKey.toBase58(),
      marketSecretKeyBase58: bs58.encode(marketKp.secretKey),
      debtMint: debtMint.toBase58(),
      collateralMint: collateralMint.toBase58(),
      debtBank: debt.address.toBase58(),
      debtLiquidityVault: debt.bank.liquidityVault.toBase58(),
      debtBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(debt.address).toBase58(),
      debtOracles: bankOracleAccounts(debt.bank).map((p) => p.toBase58()),
      ...(m.debtPythFeedIdHex ? { debtPythFeedIdHex: m.debtPythFeedIdHex } : {}),
      ...(m.debtPythShardId !== undefined ? { debtPythShardId: m.debtPythShardId } : {}),
      collateralBank: collateral.address.toBase58(),
      collateralLiquidityVault: collateral.bank.liquidityVault.toBase58(),
      collateralBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(collateral.address).toBase58(),
      collateralOracles: bankOracleAccounts(collateral.bank).map((p) => p.toBase58()),
      ...(m.collateralPythFeedIdHex ? { collateralPythFeedIdHex: m.collateralPythFeedIdHex } : {}),
      ...(m.collateralPythShardId !== undefined ? { collateralPythShardId: m.collateralPythShardId } : {}),
      marginfiGroup: marginfiGroup.toBase58(),
      signature: sig,
    };
    // Persist incrementally — if a later market fails, the operator can
    // restart and pick up from where we left off.
    writeJson('markets.json', all);
    appendTxLog({
      script: 'init-market',
      signatures: [allocateSig, sig],
      summary: {
        label: m.label,
        market: marketKp.publicKey.toBase58(),
        debtBank: debt.address.toBase58(),
        collateralBank: collateral.address.toBase58(),
      },
    });
    log(`[init-market] wrote ${m.label} to markets.json`);
  }

  // Keep the spl-token import alive against tree-shaking — downstream
  // scripts pull `TOKEN_PROGRAM_ID` through this module's import graph.
  void TOKEN_PROGRAM_ID;
}

interface ResolvedBank {
  address: PublicKey;
  bank: Bank;
}

async function readAndVerifyBank(
  conn: ReturnType<typeof loadConnection>,
  bankPubkey: string,
  expectedMint: PublicKey,
  expectedGroup: PublicKey,
  label: string,
): Promise<ResolvedBank> {
  const address = new PublicKey(bankPubkey);
  const bank = await readBankOnchain(conn, address);
  if (!bank.mint.equals(expectedMint)) {
    throw new Error(
      `markets-input.json ${label}: bank ${address.toBase58()} has mint ` +
      `${bank.mint.toBase58()} ≠ input ${expectedMint.toBase58()}`,
    );
  }
  if (!bank.group.equals(expectedGroup)) {
    throw new Error(
      `markets-input.json ${label}: bank ${address.toBase58()} group ` +
      `${bank.group.toBase58()} ≠ input.marginfiGroup ${expectedGroup.toBase58()}`,
    );
  }
  return { address, bank };
}

async function sendAllocate(
  conn: ReturnType<typeof loadConnection>,
  signer: Keypair,
  marketKp: Keypair,
  allocateIx: ReturnType<typeof SystemProgram.createAccount>,
): Promise<string> {
  // The market keypair has to sign its own create-account ix, so we can't
  // route this through `sendIxs` (which only takes a single primary
  // signer). Hand-roll the tx instead — but keep the same mainnet confirm
  // gate so this allocate can't fire ahead of a gated `create_market`.
  assertSendAllowed(conn);
  const tx = new Transaction().add(allocateIx);
  const { blockhash, lastValidBlockHeight } = await conn.getLatestBlockhash('confirmed');
  tx.recentBlockhash = blockhash;
  tx.feePayer = signer.publicKey;
  tx.sign(signer, marketKp);
  const sig = await conn.sendRawTransaction(tx.serialize());
  await conn.confirmTransaction(
    { signature: sig, blockhash, lastValidBlockHeight },
    'confirmed',
  );
  return sig;
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
