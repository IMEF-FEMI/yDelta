import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 27 — toggle `MarketFixed.is_paused`. Admin-gated. Markets ship
 * unpaused (the on-chain `FeeConfig::default()` is already safe), so
 * this ix is purely for halting / resuming a live market.
 */
export function setMarketPauseInstruction(args: {
  admin: PublicKey;
  market: PublicKey;
  paused: boolean;
}): TransactionInstruction {
  const data = new Writer()
    .u8(InstructionTag.SetMarketPause)
    .u8(args.paused ? 1 : 0)
    .toBuffer();
  return ydeltaIx(
    [signerRw(args.admin), ro(globalConfigPda()[0]), rw(args.market)],
    data,
  );
}
