import { Keypair, PublicKey } from '@solana/web3.js';
import { ProgramTestContext } from 'solana-bankrun';
import { YDELTA_PROGRAM_ID } from '../../../src/utils/programId';

/** `BpfLoaderUpgradeable` program id. */
export const BPF_LOADER_UPGRADEABLE = new PublicKey('BPFLoaderUpgradeab1e11111111111111111111111');

/** Derive `[program_id]` against `BpfLoaderUpgradeable`. */
export function programDataAddress(programId: PublicKey = YDELTA_PROGRAM_ID): PublicKey {
  return PublicKey.findProgramAddressSync([programId.toBuffer()], BPF_LOADER_UPGRADEABLE)[0];
}

/** Build a minimal `ProgramData` account body that sets `authority` as the
 *  upgrade authority. Matches the bincode layout the ydelta loader parses
 *  (`programs/ydelta/src/validation/loaders.rs::CreateGlobalConfigContext`):
 *    4   tag = 3 (ProgramData variant)
 *    8   slot (u64 LE)
 *    1   tag = 1 (Some)
 *    32  pubkey
 *  Trailing bytes (the .so bytecode) are not parsed by ydelta and are
 *  irrelevant to the upgrade-authority check — we leave them zero. */
export function buildProgramDataBuffer(authority: PublicKey, slot: number = 0): Buffer {
  const buf = Buffer.alloc(45);
  buf.writeUInt32LE(3, 0); // ProgramData variant tag
  buf.writeBigUInt64LE(BigInt(slot), 4);
  buf.writeUInt8(1, 12); // Some
  authority.toBuffer().copy(buf, 13);
  return buf;
}

/** Override the ydelta ProgramData PDA in bankrun so `payer` is recognised
 *  as the upgrade authority. Required before `CreateGlobalConfig` will pass. */
export function seedYdeltaProgramData(
  context: ProgramTestContext,
  payer: Keypair | PublicKey,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): void {
  const authority = payer instanceof Keypair ? payer.publicKey : payer;
  context.setAccount(programDataAddress(programId), {
    lamports: 1_000_000_000,
    data: buildProgramDataBuffer(authority),
    owner: BPF_LOADER_UPGRADEABLE,
    executable: false,
    rentEpoch: 0,
  });
}
