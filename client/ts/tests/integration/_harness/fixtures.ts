import {
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
} from '@solana/web3.js';
import {
  BanksClient,
  BanksTransactionResultWithMeta,
  ProgramTestContext,
} from 'solana-bankrun';
import {
  createCreateGlobalConfigInstruction,
  createCreateMarketInstruction,
  createClaimSeatInstruction,
  createSetMarketPauseInstruction,
} from '../../../src/ydelta/instructions';
import { YDELTA_PROGRAM_ID } from '../../../src/utils/programId';
import { MARKET_FIXED_SIZE } from '../../../src/utils/discriminator';
import {
  MARGINFI_PROGRAM_ID,
  MARGINFI_GROUP,
  USDC_BANK,
  USDC_MINT,
  SOL_BANK,
  WSOL_MINT,
} from './mainnet';

/** Process a tx through bankrun and return the full result+meta. */
export async function processTx(
  banksClient: BanksClient,
  tx: Transaction,
  payer: Keypair,
  signers: Keypair[] = [],
): Promise<BanksTransactionResultWithMeta> {
  tx.recentBlockhash = (await banksClient.getLatestBlockhash())![0];
  tx.feePayer = payer.publicKey;
  tx.sign(payer, ...signers);
  return banksClient.tryProcessTransaction(tx);
}

/** Process and assert success. Throws with the bankrun logs on failure. */
export async function processTxOrThrow(
  banksClient: BanksClient,
  tx: Transaction,
  payer: Keypair,
  signers: Keypair[] = [],
): Promise<BanksTransactionResultWithMeta> {
  const res = await processTx(banksClient, tx, payer, signers);
  if (res.result) {
    const logs = (res.meta?.logMessages ?? []) as string[];
    throw new Error(
      `tx failed: ${JSON.stringify(res.result)}\nlogs:\n${logs.join('\n')}`,
    );
  }
  return res;
}

/** Boot the global config PDA with `payer` as the protocol admin. Idempotent —
 *  no-op (returns null) when the PDA already exists. */
export async function createGlobalConfigIfMissing({
  context,
  payer,
}: {
  context: ProgramTestContext;
  payer: Keypair;
}): Promise<BanksTransactionResultWithMeta | null> {
  const ix = createCreateGlobalConfigInstruction({ payer: payer.publicKey });
  // Check if already exists by hitting the PDA.
  const [pda] = PublicKey.findProgramAddressSync([Buffer.from('global_config')], YDELTA_PROGRAM_ID);
  const existing = await context.banksClient.getAccount(pda);
  if (existing && existing.data.length > 0) return null;
  return processTxOrThrow(context.banksClient, new Transaction().add(ix), payer);
}

export type CreateMarketResult = {
  market: Keypair;
  signature: BanksTransactionResultWithMeta;
};

/** Create a USDC/WSOL market with `payer` as the market admin. The market
 *  account is a fresh Keypair (40-byte+ user-supplied key, per CreateMarket's
 *  PDA seed of `[b"market", market_key]`). */
export async function createUsdcWsolMarket({
  context,
  payer,
}: {
  context: ProgramTestContext;
  payer: Keypair;
}): Promise<CreateMarketResult> {
  const market = Keypair.generate();

  // Pre-allocate the market account from the system program → ydelta
  // (CreateMarket loads it as a writable, owned-by-ydelta account; the
  // Rust builder uses system_instruction::create_account first).
  const rent = await context.banksClient.getRent();
  const minBal = Number(rent.minimumBalance(BigInt(MARKET_FIXED_SIZE)));
  const allocIx = SystemProgram.createAccount({
    fromPubkey: payer.publicKey,
    newAccountPubkey: market.publicKey,
    lamports: minBal,
    space: MARKET_FIXED_SIZE,
    programId: YDELTA_PROGRAM_ID,
  });

  const createIx = createCreateMarketInstruction({
    payer: payer.publicKey,
    market: market.publicKey,
    debtMint: USDC_MINT,
    collateralMint: WSOL_MINT,
    marginfiGroup: MARGINFI_GROUP,
    debtBank: USDC_BANK,
    collateralBank: SOL_BANK,
    marginfiProgram: MARGINFI_PROGRAM_ID,
  });

  const tx = new Transaction().add(allocIx, createIx);
  const sig = await processTxOrThrow(context.banksClient, tx, payer, [market]);
  // New markets ship paused; unpause for the test suite so existing tests
  // that mutate market state through this fixture keep working.
  const unpauseIx = createSetMarketPauseInstruction(
    { admin: payer.publicKey, market: market.publicKey },
    { paused: false },
  );
  await processTxOrThrow(context.banksClient, new Transaction().add(unpauseIx), payer);
  return { market, signature: sig };
}

/** Open a user seat in the market. */
export async function claimSeat({
  context,
  payer,
  market,
}: {
  context: ProgramTestContext;
  payer: Keypair;
  market: PublicKey;
}): Promise<BanksTransactionResultWithMeta> {
  const ix = createClaimSeatInstruction({ payer: payer.publicKey, market });
  return processTxOrThrow(context.banksClient, new Transaction().add(ix), payer);
}
