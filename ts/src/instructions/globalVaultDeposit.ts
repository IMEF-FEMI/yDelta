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
 * Tag 10 — `GlobalVaultDeposit`. Atoms route depositor wallet → staging →
 * vault integration account. The depositor's stake in the chosen
 * `profileId` grows by the freshly-minted vault shares.
 */
export interface GlobalVaultDepositArgs {
  depositor: PublicKey;
  mint: PublicKey;
  depositorToken: PublicKey;
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  /** marginfi `Bank` pinned at vault creation — must equal vault.lendingPool. */
  lendingPool: PublicKey;
  /** marginfi liquidity vault for the pinned bank. */
  liquidityVault: PublicKey;
  marginfiProgram: PublicKey;
  profileId: number;
  amountAtoms: bigint | BN | number;
}

export function globalVaultDepositInstruction(
  args: GlobalVaultDepositArgs,
): TransactionInstruction {
  const vault = globalVaultPda(args.mint)[0];
  const signerPda = globalVaultSignerPda(vault)[0];
  const staging = globalVaultStagingPda(vault)[0];
  const integration = globalVaultIntegrationAccountPda(vault)[0];
  const data = new Writer()
    .u8(InstructionTag.GlobalVaultDeposit)
    .u64(args.amountAtoms)
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
      rw(args.liquidityVault),
      ro(args.marginfiProgram),
      rw(userAccountPda(args.depositor)[0]),
      ro(SystemProgram.programId),
    ],
    data,
  );
}
