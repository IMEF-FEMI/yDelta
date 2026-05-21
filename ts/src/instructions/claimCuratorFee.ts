import { PublicKey, TransactionInstruction } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  globalConfigPda,
  globalVaultIntegrationAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
} from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 15 — `ClaimCuratorFee`. Drains
 * `RiskProfile.accumulated_curator_fee_atoms` to the curator's wallet ATA
 * via `marginfi.withdraw + SPL transfer`. No-op (Ok) when zero, so curators
 * can poll.
 */
export interface ClaimCuratorFeeArgs {
  curator: PublicKey;
  mint: PublicKey;
  profileId: number;
  curatorToken: PublicKey;
  debtBank: PublicKey;
  liquidityVault: PublicKey;
  bankLiquidityVaultAuthority: PublicKey;
  bankOracle: PublicKey;
  marginfiProgram: PublicKey;
  marginfiGroup: PublicKey;
  /** Token program owning the vault mint. Defaults to classic SPL. */
  tokenProgram?: PublicKey;
}

export function claimCuratorFeeInstruction(
  args: ClaimCuratorFeeArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const signerPda = globalVaultSignerPda(vault)[0];
  const staging = globalVaultStagingPda(vault)[0];
  const integration = globalVaultIntegrationAccountPda(vault)[0];
  const data = new Writer()
    .u8(InstructionTag.ClaimCuratorFee)
    .u8(args.profileId)
    .toBuffer();
  const keys = [
    signerRw(args.curator),
    ro(globalConfigPda()[0]),
    rw(vault),
    ro(signerPda),
    rw(staging),
    rw(integration),
    rw(args.curatorToken),
    rw(args.debtBank),
    rw(args.liquidityVault),
    ro(args.bankLiquidityVaultAuthority),
    ro(args.bankOracle),
    ro(args.mint),
    ro(args.tokenProgram ?? TOKEN_PROGRAM_ID),
    ro(args.marginfiProgram),
    ro(args.marginfiGroup),
  ];
  return ydeltaIx(keys, data);
}
