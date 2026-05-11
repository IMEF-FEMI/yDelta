import {
  AccountLayout,
  ASSOCIATED_TOKEN_PROGRAM_ID,
  MINT_SIZE,
  MintLayout,
  TOKEN_PROGRAM_ID,
  createAssociatedTokenAccountInstruction,
  createMintToInstruction,
  getAssociatedTokenAddressSync,
} from '@solana/spl-token';
import {
  Keypair,
  PublicKey,
  Transaction,
} from '@solana/web3.js';
import { ProgramTestContext, BanksClient } from 'solana-bankrun';
import { processTxOrThrow } from './fixtures';
import { USDC_MINT, WSOL_MINT } from './mainnet';

/** Override a mainnet-loaded mint so `newAuthority` becomes its mint authority.
 *  Required because we replay mainnet USDC/WSOL state but don't control the
 *  real mainnet authorities. Mirrors the trick from marginfi-v2 tests where
 *  the test's bankrun payer is used as the mint authority. */
export function overrideMintAuthority({
  context,
  mint,
  newAuthority,
  decimalsOverride,
}: {
  context: ProgramTestContext;
  mint: PublicKey;
  newAuthority: PublicKey;
  /** Override decimals (when the mint isn't pre-loaded). Defaults to reading
   *  the current account's decimals byte. */
  decimalsOverride?: number;
}): void {
  // SPL Mint layout (82 bytes):
  //   4   mint_authority_option (1 = Some)
  //   32  mint_authority
  //   8   supply (u64)
  //   1   decimals
  //   1   is_initialized
  //   4   freeze_authority_option
  //   32  freeze_authority
  const buf = Buffer.alloc(MINT_SIZE);
  buf.writeUInt32LE(1, 0); // Some
  newAuthority.toBuffer().copy(buf, 4);
  // supply = 0 (we'll mint into ATAs after; this only counts atoms minted via
  // this mint going forward)
  buf.writeBigUInt64LE(0n, 36);
  const decimals = decimalsOverride !== undefined ? decimalsOverride : 6; // USDC default
  buf.writeUInt8(decimals, 44);
  buf.writeUInt8(1, 45); // is_initialized
  buf.writeUInt32LE(0, 46); // freeze_authority_option = None
  context.setAccount(mint, {
    lamports: 1_000_000_000,
    data: buf,
    owner: TOKEN_PROGRAM_ID,
    executable: false,
    rentEpoch: 0,
  });
}

/** Override USDC (6 decimals) and WSOL (9 decimals) mainnet mints so `authority`
 *  can mintTo any ATA. */
export function overrideUsdcWsolAuthorities({
  context,
  authority,
}: {
  context: ProgramTestContext;
  authority: PublicKey;
}): void {
  overrideMintAuthority({ context, mint: USDC_MINT, newAuthority: authority, decimalsOverride: 6 });
  overrideMintAuthority({ context, mint: WSOL_MINT, newAuthority: authority, decimalsOverride: 9 });
}

/** Create an ATA for `owner` on `mint` and mint `amountAtoms` into it.
 *  `mintAuthority` must currently own the mint (override via
 *  `overrideMintAuthority` if needed). Returns the ATA address. */
export async function createAtaAndMintTo({
  context,
  payer,
  owner,
  mint,
  mintAuthority,
  amountAtoms,
}: {
  context: ProgramTestContext;
  payer: Keypair;
  owner: PublicKey;
  mint: PublicKey;
  mintAuthority: Keypair;
  amountAtoms: bigint;
}): Promise<PublicKey> {
  const ata = getAssociatedTokenAddressSync(mint, owner, /*allowOwnerOffCurve=*/ true);
  const tx = new Transaction().add(
    createAssociatedTokenAccountInstruction(payer.publicKey, ata, owner, mint),
    createMintToInstruction(mint, ata, mintAuthority.publicKey, amountAtoms),
  );
  await processTxOrThrow(
    context.banksClient,
    tx,
    payer,
    mintAuthority.publicKey.equals(payer.publicKey) ? [] : [mintAuthority],
  );
  return ata;
}

/** Read an SPL token account's raw `amount` field. Returns 0 when missing. */
export async function getTokenBalance(
  banks: BanksClient,
  tokenAccount: PublicKey,
): Promise<bigint> {
  const info = await banks.getAccount(tokenAccount);
  if (!info) return 0n;
  const decoded = AccountLayout.decode(Buffer.from(info.data));
  return decoded.amount;
}

/** Fund an ATA directly by overwriting its account data — bypasses the mint
 *  authority entirely. Useful when the mint can't be authority-overridden. */
export function seedTokenAccount({
  context,
  tokenAccount,
  mint,
  owner,
  amount,
}: {
  context: ProgramTestContext;
  tokenAccount: PublicKey;
  mint: PublicKey;
  owner: PublicKey;
  amount: bigint;
}): void {
  const buf = Buffer.alloc(AccountLayout.span);
  AccountLayout.encode(
    {
      mint,
      owner,
      amount,
      delegateOption: 0,
      delegate: PublicKey.default,
      state: 1, // initialized
      isNativeOption: 0,
      isNative: 0n,
      delegatedAmount: 0n,
      closeAuthorityOption: 0,
      closeAuthority: PublicKey.default,
    },
    buf,
  );
  context.setAccount(tokenAccount, {
    lamports: 2_039_280,
    data: buf,
    owner: TOKEN_PROGRAM_ID,
    executable: false,
    rentEpoch: 0,
  });
}

void ASSOCIATED_TOKEN_PROGRAM_ID;
void MintLayout;
