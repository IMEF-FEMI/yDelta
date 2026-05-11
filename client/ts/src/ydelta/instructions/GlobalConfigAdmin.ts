import * as beet from '@metaplex-foundation/beet';
import { publicKey as beetPublicKey } from '@metaplex-foundation/beet-solana';
import {
  AccountMeta,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import { globalConfigPda } from '../../utils/pdas';

// `BPF Loader Upgradeable` program ID is fixed.
const BPF_LOADER_UPGRADEABLE_ID = new PublicKey('BPFLoaderUpgradeab1e11111111111111111111111');

function programDataAddress(programId: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync(
    [programId.toBuffer()],
    BPF_LOADER_UPGRADEABLE_ID,
  )[0];
}

// ─── CreateGlobalConfig (tag 32) ───

export const createGlobalConfigInstructionDiscriminator = YdeltaInstructionTag.CreateGlobalConfig;

export type CreateGlobalConfigInstructionAccounts = {
  payer: PublicKey;
};

export function createCreateGlobalConfigInstruction(
  accounts: CreateGlobalConfigInstructionAccounts,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [globalConfig] = globalConfigPda(programId);
  const programData = programDataAddress(programId);
  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfig, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
    { pubkey: programData, isWritable: false, isSigner: false },
  ];
  return new TransactionInstruction({
    programId,
    keys,
    data: Buffer.from([createGlobalConfigInstructionDiscriminator]),
  });
}

// ─── TransferProtocolAdmin (tag 33) ───

export const transferProtocolAdminInstructionDiscriminator = YdeltaInstructionTag.TransferProtocolAdmin;

const TransferProtocolStruct = new beet.BeetArgsStruct<{
  instructionDiscriminator: number;
  newAdmin: PublicKey;
}>(
  [
    ['instructionDiscriminator', beet.u8],
    ['newAdmin', beetPublicKey],
  ],
  'TransferProtocolAdminInstructionArgs',
);

export type TransferProtocolAdminInstructionAccounts = {
  currentAdmin: PublicKey;
};

export function createTransferProtocolAdminInstruction(
  accounts: TransferProtocolAdminInstructionAccounts,
  args: { newAdmin: PublicKey },
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = TransferProtocolStruct.serialize({
    instructionDiscriminator: transferProtocolAdminInstructionDiscriminator,
    ...args,
  });
  const [globalConfig] = globalConfigPda(programId);
  const keys: AccountMeta[] = [
    { pubkey: accounts.currentAdmin, isWritable: true, isSigner: true },
    { pubkey: globalConfig, isWritable: true, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}

// ─── AcceptProtocolAdmin (tag 34) ───

export const acceptProtocolAdminInstructionDiscriminator = YdeltaInstructionTag.AcceptProtocolAdmin;

export type AcceptProtocolAdminInstructionAccounts = {
  pendingAdmin: PublicKey;
};

export function createAcceptProtocolAdminInstruction(
  accounts: AcceptProtocolAdminInstructionAccounts,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [globalConfig] = globalConfigPda(programId);
  const keys: AccountMeta[] = [
    { pubkey: accounts.pendingAdmin, isWritable: true, isSigner: true },
    { pubkey: globalConfig, isWritable: true, isSigner: false },
  ];
  return new TransactionInstruction({
    programId,
    keys,
    data: Buffer.from([acceptProtocolAdminInstructionDiscriminator]),
  });
}

// ─── SetGlobalPause (tag 35) ───

export const setGlobalPauseInstructionDiscriminator = YdeltaInstructionTag.SetGlobalPause;

const SetGlobalPauseStruct = new beet.BeetArgsStruct<{
  instructionDiscriminator: number;
  paused: number;
}>(
  [
    ['instructionDiscriminator', beet.u8],
    ['paused', beet.u8],
  ],
  'SetGlobalPauseInstructionArgs',
);

export type SetGlobalPauseInstructionAccounts = {
  admin: PublicKey;
};

export function createSetGlobalPauseInstruction(
  accounts: SetGlobalPauseInstructionAccounts,
  args: { paused: boolean },
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = SetGlobalPauseStruct.serialize({
    instructionDiscriminator: setGlobalPauseInstructionDiscriminator,
    paused: args.paused ? 1 : 0,
  });
  const [globalConfig] = globalConfigPda(programId);
  const keys: AccountMeta[] = [
    { pubkey: accounts.admin, isWritable: true, isSigner: true },
    { pubkey: globalConfig, isWritable: true, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}
