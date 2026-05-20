import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';

import { globalConfigPda } from '../pdas.js';
import { Writer } from './_serialise.js';
import { ro, signerRw, ydeltaIx } from './_helpers.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 28 — `CreateGlobalConfig`.
 *
 * One-shot. Allocates `[b"global_config"]` and stamps the signer as the
 * initial `protocol_admin`. `program_data` is the `BpfLoaderUpgradeable`
 * ProgramData account — the deployer is gated against the upgrade authority.
 */
export interface CreateGlobalConfigArgs {
  payer: PublicKey;
  /**
   * The deployer's `program_data` account — derived as
   * `findProgramAddressSync([programId.toBuffer()], BpfLoaderUpgradeable)`.
   * Callers are responsible for passing the right PDA; the SDK does not
   * derive it (it requires the BPF upgradeable loader id).
   */
  programData: PublicKey;
}

export function createGlobalConfigInstruction(args: CreateGlobalConfigArgs): TransactionInstruction {
  const data = new Writer().u8(InstructionTag.CreateGlobalConfig).toBuffer();
  const keys = [
    signerRw(args.payer),
    { pubkey: globalConfigPda()[0], isSigner: false, isWritable: true },
    ro(SystemProgram.programId),
    ro(args.programData),
  ];
  return ydeltaIx(keys, data);
}
