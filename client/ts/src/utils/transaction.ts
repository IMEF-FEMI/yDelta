import {
  AddressLookupTableAccount,
  ComputeBudgetProgram,
  Connection,
  Keypair,
  PublicKey,
  Signer,
  Transaction,
  TransactionInstruction,
  TransactionMessage,
  VersionedTransaction,
  sendAndConfirmTransaction,
} from '@solana/web3.js';

/** Account count above which the composer prefers v0 transactions. Legacy
 *  tx size is hard-capped by the 1232-byte packet limit, not by a flat key
 *  count — but every account meta costs ~32 bytes (pubkey) + bits of the
 *  compact-u16 prefix, so ~32 keys is a safe headroom-friendly threshold
 *  once you add signer + ATA + ComputeBudget ixs to a tx. */
export const LEGACY_TX_MAX_KEYS = 32;

export type BuildV0TxArgs = {
  payer: PublicKey;
  ixs: TransactionInstruction[];
  recentBlockhash: string;
  luts?: AddressLookupTableAccount[];
  priorityFeeMicroLamports?: number;
  computeUnitLimit?: number;
};

export function buildV0Tx({
  payer,
  ixs,
  recentBlockhash,
  luts,
  priorityFeeMicroLamports,
  computeUnitLimit,
}: BuildV0TxArgs): VersionedTransaction {
  const all = prependComputeBudget(ixs, computeUnitLimit, priorityFeeMicroLamports);
  const message = new TransactionMessage({
    payerKey: payer,
    recentBlockhash,
    instructions: all,
  }).compileToV0Message(luts ?? []);
  return new VersionedTransaction(message);
}

function prependComputeBudget(
  ixs: TransactionInstruction[],
  computeUnitLimit?: number,
  priorityFeeMicroLamports?: number,
): TransactionInstruction[] {
  const all: TransactionInstruction[] = [];
  if (computeUnitLimit !== undefined) {
    all.push(ComputeBudgetProgram.setComputeUnitLimit({ units: computeUnitLimit }));
  }
  if (priorityFeeMicroLamports !== undefined) {
    all.push(
      ComputeBudgetProgram.setComputeUnitPrice({
        microLamports: priorityFeeMicroLamports,
      }),
    );
  }
  all.push(...ixs);
  return all;
}

export type SendArgs = {
  connection: Connection;
  payer: Keypair;
  ixs: TransactionInstruction[];
  signers?: Signer[];
  skipPreflight?: boolean;
  priorityFeeMicroLamports?: number;
  computeUnitLimit?: number;
};

export async function sendLegacyTx({
  connection,
  payer,
  ixs,
  signers = [],
  skipPreflight = false,
  priorityFeeMicroLamports,
  computeUnitLimit,
}: SendArgs): Promise<string> {
  const all = prependComputeBudget(ixs, computeUnitLimit, priorityFeeMicroLamports);
  const tx = new Transaction().add(...all);
  return sendAndConfirmTransaction(connection, tx, [payer, ...signers], {
    skipPreflight,
    commitment: 'confirmed',
  });
}

export type SendV0Args = {
  connection: Connection;
  payer: Keypair;
  ixs: TransactionInstruction[];
  luts?: AddressLookupTableAccount[];
  signers?: Signer[];
  skipPreflight?: boolean;
  priorityFeeMicroLamports?: number;
  computeUnitLimit?: number;
};

/** Compose and send a v0 tx with optional LUTs + compute-budget ix. */
export async function sendV0Tx({
  connection,
  payer,
  ixs,
  luts,
  signers = [],
  skipPreflight = false,
  priorityFeeMicroLamports,
  computeUnitLimit,
}: SendV0Args): Promise<string> {
  const { blockhash, lastValidBlockHeight } = await connection.getLatestBlockhash('confirmed');
  const tx = buildV0Tx({
    payer: payer.publicKey,
    ixs,
    recentBlockhash: blockhash,
    luts,
    priorityFeeMicroLamports,
    computeUnitLimit,
  });
  tx.sign([payer, ...signers]);
  const signature = await connection.sendTransaction(tx, { skipPreflight });
  await connection.confirmTransaction(
    { signature, blockhash, lastValidBlockHeight },
    'confirmed',
  );
  return signature;
}

/** Count unique account keys across `ixs` (including their `programId`s). */
export function countUniqueKeys(ixs: TransactionInstruction[]): number {
  const seen = new Set<string>();
  for (const ix of ixs) {
    seen.add(ix.programId.toBase58());
    for (const k of ix.keys) seen.add(k.pubkey.toBase58());
  }
  return seen.size;
}

/** Pick legacy or v0 based on key count. Pass `luts` to bias toward v0 (the
 *  v0 form is always preferred when LUTs are supplied). */
export type AutoTxMode = 'legacy' | 'v0';
export function pickTxMode(
  ixs: TransactionInstruction[],
  luts?: AddressLookupTableAccount[],
): AutoTxMode {
  if (luts && luts.length > 0) return 'v0';
  return countUniqueKeys(ixs) > LEGACY_TX_MAX_KEYS ? 'v0' : 'legacy';
}

/** Send a tx using whichever wire format is appropriate. */
export async function sendAutoTx(args: SendV0Args): Promise<string> {
  const mode = pickTxMode(args.ixs, args.luts);
  if (mode === 'v0') return sendV0Tx(args);
  return sendLegacyTx(args);
}
