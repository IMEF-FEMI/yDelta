import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda, globalVaultPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 9 — `CreateRiskProfile`. Vault-admin-gated. Realloc-grows the vault
 * account for a fresh 512-byte profile block + inserts a `RiskProfile` tree
 * node.
 *
 * `profile_id` is **assigned by the program** from the vault's monotonic
 * `next_profile_id` counter (1-based; 0 is the sentinel/invalid). Callers
 * cannot request a specific id. The assigned id is reported back via
 * `RiskProfileCreatedLog.profile_id`.
 *
 * `maxLtvBps` is `Option<u16>` on the wire:
 *   - `number` (must be in `(0, 10_000)`) — fixed LTV cap, enforced as-is.
 *   - `null` / `undefined` — defers to the live marginfi bank weights at
 *     match time: cap = `floor(coll_asset_weight / debt_liability_weight ×
 *     10_000) - LTV_AUTO_BUFFER_BPS`. The on-chain field stores the
 *     `LTV_AUTO_FROM_MARGINFI` sentinel (65535) in that case.
 */
export interface CreateRiskProfileArgs {
  payer: PublicKey;
  mint: PublicKey;
  curator: PublicKey;
  /** `null`/`undefined` → marginfi-derived at match time. `number` → fixed cap in `(0, 10_000)`. */
  maxLtvBps: number | null;
  maxTermSeconds: number;
}

export function createRiskProfileInstruction(
  args: CreateRiskProfileArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const data = new Writer()
    .u8(InstructionTag.CreateRiskProfile)
    .pubkey(args.curator)
    .optionU16(args.maxLtvBps)
    .u32(args.maxTermSeconds)
    .toBuffer();
  return ydeltaIx(
    [
      signerRw(args.payer),
      ro(globalConfigPda()[0]),
      rw(vault),
      ro(SystemProgram.programId),
    ],
    data,
  );
}
