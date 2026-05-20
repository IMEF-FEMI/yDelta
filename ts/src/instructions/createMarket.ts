import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';
import { TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  borrowerIntegrationAccountPda,
  globalConfigPda,
  lenderIntegrationAccountPda,
  marketSignerPda,
  marketTokenVaultPda,
} from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 0 — `CreateMarket`. Signer becomes `MarketFixed.admin` AND pays both
 * marginfi-account rents via CPI in the processor.
 *
 * The market account itself must be allocated by a separate
 * `SystemProgram.createAccount` ix at `MARKET_FIXED_SIZE = 512` bytes, owned
 * by the yDelta program. Callers typically bundle the two ixs in one tx.
 *
 * Every `*Bps` / `gracePeriodSeconds` field is optional and layers on top
 * of the on-chain `FeeConfig::default()` (which seeds a safe
 * `ltvBufferBps = 200` and a 24-hour `gracePeriodSeconds`). Pass only the
 * fields you want to override; omit / null the rest.
 *
 * Wire format MUST match the Rust `CreateMarketParams` borsh layout — the
 * sequence of `optionU16` / `optionU32` calls below is load-bearing.
 */
export interface CreateMarketParams {
  protocolFeeBpsFloor?: number | null;
  originationBps?: number | null;
  curatorSplitBps?: number | null;
  curatorFeeBps?: number | null;
  liquidationKeeperBps?: number | null;
  liquidationProtocolBps?: number | null;
  ltvBufferBps?: number | null;
  gracePeriodSeconds?: number | null;
}

export interface CreateMarketArgs {
  marketCreator: PublicKey;
  market: PublicKey;
  debtMint: PublicKey;
  collateralMint: PublicKey;
  marginfiGroup: PublicKey;
  debtBank: PublicKey;
  collateralBank: PublicKey;
  marginfiProgram: PublicKey;
  /** Optional fee_config overrides. Omit for protocol defaults. */
  params?: CreateMarketParams;
}

export function createMarketInstruction(args: CreateMarketArgs): TransactionInstruction {
  const debtVault = marketTokenVaultPda(args.market, args.debtMint)[0];
  const collateralVault = marketTokenVaultPda(args.market, args.collateralMint)[0];
  const lenderMa = lenderIntegrationAccountPda(args.market)[0];
  const borrowerMa = borrowerIntegrationAccountPda(args.market)[0];
  const marketSigner = marketSignerPda(args.market)[0];
  const params = args.params ?? {};
  const data = new Writer()
    .u8(InstructionTag.CreateMarket)
    .optionU16(params.protocolFeeBpsFloor ?? null)
    .optionU16(params.originationBps ?? null)
    .optionU16(params.curatorSplitBps ?? null)
    .optionU16(params.curatorFeeBps ?? null)
    .optionU16(params.liquidationKeeperBps ?? null)
    .optionU16(params.liquidationProtocolBps ?? null)
    .optionU16(params.ltvBufferBps ?? null)
    .optionU32(params.gracePeriodSeconds ?? null)
    .toBuffer();
  return ydeltaIx(
    [
      signerRw(args.marketCreator),
      ro(globalConfigPda()[0]),
      rw(args.market),
      ro(SystemProgram.programId),
      ro(args.debtMint),
      ro(args.collateralMint),
      rw(debtVault),
      rw(collateralVault),
      ro(TOKEN_PROGRAM_ID),
      ro(TOKEN_2022_PROGRAM_ID),
      ro(args.marginfiGroup),
      ro(args.debtBank),
      ro(args.collateralBank),
      rw(lenderMa),
      rw(borrowerMa),
      ro(marketSigner),
      ro(args.marginfiProgram),
    ],
    data,
  );
}
