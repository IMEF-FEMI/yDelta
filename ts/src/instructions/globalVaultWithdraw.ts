import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';
import BN from 'bn.js';

import {
  globalConfigPda,
  globalVaultIntegrationAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
  userAccountPda,
} from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 11 — `GlobalVaultWithdraw`. Reverts if
 * `profile.idle_principal_atoms < atoms_out` — deployed liquidity is
 * non-redeemable until the underlying loans repay.
 *
 * `sharesToBurn` is u128 (the on-chain `total_shares` slot is u128 to
 * accommodate fp48 share growth). UI helpers convert atoms↔shares via
 * `conversions.atomsToVaultShares` / `vaultSharesToAtoms`.
 */
export interface GlobalVaultWithdrawArgs {
  depositor: PublicKey;
  mint: PublicKey;
  depositorToken: PublicKey;
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  lendingPool: PublicKey;
  lendingPoolOracle: PublicKey;
  liquidityVault: PublicKey;
  bankLiquidityVaultAuthority: PublicKey;
  marginfiProgram: PublicKey;
  profileId: number;
  sharesToBurn: bigint | BN;
}

export function globalVaultWithdrawInstruction(
  args: GlobalVaultWithdrawArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const signerPda = globalVaultSignerPda(vault)[0];
  const staging = globalVaultStagingPda(vault)[0];
  const integration = globalVaultIntegrationAccountPda(vault)[0];
  const data = new Writer()
    .u8(InstructionTag.GlobalVaultWithdraw)
    .u128(args.sharesToBurn)
    .u8(args.profileId)
    .toBuffer();
  return ydeltaIx(
    [
      signerRw(args.depositor),
      ro(globalConfigPda()[0]),
      rw(vault),
      ro(args.mint),
      ro(signerPda),
      rw(staging),
      rw(args.depositorToken),
      ro(args.tokenProgram),
      ro(args.marginfiGroup),
      rw(integration),
      rw(args.lendingPool),
      ro(args.lendingPoolOracle),
      rw(args.liquidityVault),
      ro(args.bankLiquidityVaultAuthority),
      ro(args.marginfiProgram),
      rw(userAccountPda(args.depositor)[0]),
      ro(SystemProgram.programId),
    ],
    data,
  );
}
