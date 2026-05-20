import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import {
  globalConfigPda,
  lenderIntegrationAccountPda,
  marketSignerPda,
  marketTokenVaultPda,
} from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 19 — `ProtocolFeeClaim`. Drains
 * `market.accumulated_protocol_fee_shares` to the admin's wallet ATA. No-op
 * (Ok) when zero, so admins can poll.
 */
export interface ProtocolFeeClaimArgs {
  admin: PublicKey;
  market: PublicKey;
  debtMint: PublicKey;
  adminDebtToken: PublicKey;
  debtBank: PublicKey;
  debtLiquidityVault: PublicKey;
  debtBankLiquidityVaultAuthority: PublicKey;
  debtOracles: PublicKey[];
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
}

export function protocolFeeClaimInstruction(
  args: ProtocolFeeClaimArgs,
): TransactionInstruction {
  const marketDebtVault = marketTokenVaultPda(args.market, args.debtMint)[0];
  const marketSigner = marketSignerPda(args.market)[0];
  const lenderMa = lenderIntegrationAccountPda(args.market)[0];
  const data = new Writer().u8(InstructionTag.ProtocolFeeClaim).toBuffer();
  return ydeltaIx(
    [
      signerRw(args.admin),
      ro(globalConfigPda()[0]),
      rw(args.market),
      rw(args.adminDebtToken),
      rw(marketDebtVault),
      ro(marketSigner),
      rw(lenderMa),
      rw(args.debtBank),
      rw(args.debtLiquidityVault),
      ro(args.debtBankLiquidityVaultAuthority),
      ...args.debtOracles.map(ro),
      ro(args.debtMint),
      ro(args.tokenProgram),
      ro(args.marginfiGroup),
      ro(args.marginfiProgram),
    ],
    data,
  );
}
