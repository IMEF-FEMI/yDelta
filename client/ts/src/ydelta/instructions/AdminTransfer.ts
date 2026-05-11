import * as beet from '@metaplex-foundation/beet';
import { publicKey as beetPublicKey } from '@metaplex-foundation/beet-solana';
import {
  AccountMeta,
  PublicKey,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import { globalConfigPda, globalVaultPda } from '../../utils/pdas';

// ─── TransferMarketAdmin (tag 25) ───

export const transferMarketAdminInstructionDiscriminator = YdeltaInstructionTag.TransferMarketAdmin;

const TransferMarketAdminStruct = new beet.BeetArgsStruct<{
  instructionDiscriminator: number;
  newAdmin: PublicKey;
}>(
  [
    ['instructionDiscriminator', beet.u8],
    ['newAdmin', beetPublicKey],
  ],
  'TransferMarketAdminInstructionArgs',
);

export type TransferMarketAdminInstructionAccounts = {
  currentAdmin: PublicKey;
  market: PublicKey;
};

export function createTransferMarketAdminInstruction(
  accounts: TransferMarketAdminInstructionAccounts,
  args: { newAdmin: PublicKey },
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = TransferMarketAdminStruct.serialize({
    instructionDiscriminator: transferMarketAdminInstructionDiscriminator,
    ...args,
  });
  const keys: AccountMeta[] = [
    { pubkey: accounts.currentAdmin, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}

// ─── AcceptMarketAdmin (tag 26) ───

export const acceptMarketAdminInstructionDiscriminator = YdeltaInstructionTag.AcceptMarketAdmin;

export type AcceptMarketAdminInstructionAccounts = {
  pendingAdmin: PublicKey;
  market: PublicKey;
};

export function createAcceptMarketAdminInstruction(
  accounts: AcceptMarketAdminInstructionAccounts,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const keys: AccountMeta[] = [
    { pubkey: accounts.pendingAdmin, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
  ];
  return new TransactionInstruction({
    programId,
    keys,
    data: Buffer.from([acceptMarketAdminInstructionDiscriminator]),
  });
}

// ─── TransferGlobalVaultAdmin (tag 27) ───

export const transferGlobalVaultAdminInstructionDiscriminator = YdeltaInstructionTag.TransferGlobalVaultAdmin;

const TransferGvAdminStruct = new beet.BeetArgsStruct<{
  instructionDiscriminator: number;
  newAdmin: PublicKey;
}>(
  [
    ['instructionDiscriminator', beet.u8],
    ['newAdmin', beetPublicKey],
  ],
  'TransferGlobalVaultAdminInstructionArgs',
);

export type TransferGlobalVaultAdminInstructionAccounts = {
  currentAdmin: PublicKey;
  mint: PublicKey;
};

export function createTransferGlobalVaultAdminInstruction(
  accounts: TransferGlobalVaultAdminInstructionAccounts,
  args: { newAdmin: PublicKey },
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = TransferGvAdminStruct.serialize({
    instructionDiscriminator: transferGlobalVaultAdminInstructionDiscriminator,
    ...args,
  });
  const [vault] = globalVaultPda(accounts.mint, programId);
  const keys: AccountMeta[] = [
    { pubkey: accounts.currentAdmin, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: vault, isWritable: true, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}

// ─── AcceptGlobalVaultAdmin (tag 28) ───

export const acceptGlobalVaultAdminInstructionDiscriminator = YdeltaInstructionTag.AcceptGlobalVaultAdmin;

export type AcceptGlobalVaultAdminInstructionAccounts = {
  pendingAdmin: PublicKey;
  mint: PublicKey;
};

export function createAcceptGlobalVaultAdminInstruction(
  accounts: AcceptGlobalVaultAdminInstructionAccounts,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [vault] = globalVaultPda(accounts.mint, programId);
  const keys: AccountMeta[] = [
    { pubkey: accounts.pendingAdmin, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: vault, isWritable: true, isSigner: false },
  ];
  return new TransactionInstruction({
    programId,
    keys,
    data: Buffer.from([acceptGlobalVaultAdminInstructionDiscriminator]),
  });
}

// ─── TransferCurator (tag 29) ───

export const transferCuratorInstructionDiscriminator = YdeltaInstructionTag.TransferCurator;

const TransferCuratorStruct = new beet.BeetArgsStruct<{
  instructionDiscriminator: number;
  profileId: number;
  newCurator: PublicKey;
}>(
  [
    ['instructionDiscriminator', beet.u8],
    ['profileId', beet.u8],
    ['newCurator', beetPublicKey],
  ],
  'TransferCuratorInstructionArgs',
);

export type TransferCuratorInstructionAccounts = {
  currentCurator: PublicKey;
  mint: PublicKey;
};

export function createTransferCuratorInstruction(
  accounts: TransferCuratorInstructionAccounts,
  args: { profileId: number; newCurator: PublicKey },
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = TransferCuratorStruct.serialize({
    instructionDiscriminator: transferCuratorInstructionDiscriminator,
    ...args,
  });
  const [vault] = globalVaultPda(accounts.mint, programId);
  const keys: AccountMeta[] = [
    { pubkey: accounts.currentCurator, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: vault, isWritable: true, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}

// ─── AcceptCurator (tag 30) ───

export const acceptCuratorInstructionDiscriminator = YdeltaInstructionTag.AcceptCurator;

const AcceptCuratorStruct = new beet.BeetArgsStruct<{
  instructionDiscriminator: number;
  profileId: number;
}>(
  [
    ['instructionDiscriminator', beet.u8],
    ['profileId', beet.u8],
  ],
  'AcceptCuratorInstructionArgs',
);

export type AcceptCuratorInstructionAccounts = {
  pendingCurator: PublicKey;
  mint: PublicKey;
};

export function createAcceptCuratorInstruction(
  accounts: AcceptCuratorInstructionAccounts,
  args: { profileId: number },
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = AcceptCuratorStruct.serialize({
    instructionDiscriminator: acceptCuratorInstructionDiscriminator,
    ...args,
  });
  const [vault] = globalVaultPda(accounts.mint, programId);
  const keys: AccountMeta[] = [
    { pubkey: accounts.pendingCurator, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: vault, isWritable: true, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}
