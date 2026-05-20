import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 27 — toggle `MarketFixed.is_paused`. Admin-gated; gated on
 * `fee_config_set` for the un-pause case (markets ship paused-by-default
 * and the admin must call `set_fee_config` once before unpausing).
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
