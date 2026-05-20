import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signer, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 12 — `PlaceOrderForRiskProfile`. Zero-CPI curator quote. Pure-memory
 * bookkeeping: inserts a `RestingOrder` into the market's asks tree + a
 * `RiskProfileOrderRef` into the vault. Auto-creates the vault-owned
 * `ClaimedSeat` for this `(vault, profile_id)` on first call per market.
 *
 * Uses the **split-payer** pattern: `feePayer` covers tx fees + any realloc
 * rent; `curator` is the verifying signer (must equal `profile.curator`).
 * Pass the same key for both when they're the same wallet.
 */
export interface PlaceOrderForRiskProfileArgs {
  feePayer: PublicKey;
  curator: PublicKey;
  mint: PublicKey;
  market: PublicKey;
  profileId: number;
  rateBps: number;
  termSeconds: number;
  /** Reserved for future ask-side flags. Pass `0` unless you know otherwise. */
  flags?: number;
}

export function placeOrderForRiskProfileInstruction(
  args: PlaceOrderForRiskProfileArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.PlaceOrderForRiskProfile)
    .u8(args.profileId)
    .u16(args.rateBps)
    .u32(args.termSeconds)
    .u8(args.flags ?? 0)
    .toBuffer();
  return ydeltaIx(
    [
      signerRw(args.feePayer),
      signer(args.curator),
      ro(globalConfigPda()[0]),
      rw(vault),
      rw(args.market),
      ro(SystemProgram.programId),
    ],
    data,
  );
}
