import { NIL, RBTREE_OVERHEAD_BYTES } from './discriminator';

export type RBNodeHeader = {
  left: number;
  right: number;
  parent: number;
  color: number;
  payloadType: number;
};

export function readRBNodeHeader(buf: Buffer, offset: number): RBNodeHeader {
  return {
    left: buf.readUInt32LE(offset + 0),
    right: buf.readUInt32LE(offset + 4),
    parent: buf.readUInt32LE(offset + 8),
    color: buf.readUInt8(offset + 12),
    payloadType: buf.readUInt8(offset + 13),
    // bytes 14..16 — unused padding
  };
}

export type RBPayloadDeserializer<T> = (
  payload: Buffer,
  /** Block offset within `dynamic` (after fixed header), useful for back-refs. */
  blockOffset: number,
) => T;

/** In-order iteration of a hypertree red-black tree whose nodes live in `dynamic`
 *  on `blockSize`-byte boundaries. The first 16 bytes of every block are the
 *  `RBNode` header; the remaining `blockSize - 16` bytes are the payload.
 *  Yields nodes in ascending `Ord` order. */
export function* inOrderTraverse<T>(
  dynamic: Buffer,
  rootIndex: number,
  blockSize: number,
  decode: RBPayloadDeserializer<T>,
): Generator<T> {
  if (rootIndex === NIL) return;
  const stack: number[] = [];
  let cur = rootIndex;
  while (cur !== NIL || stack.length > 0) {
    while (cur !== NIL) {
      stack.push(cur);
      cur = dynamic.readUInt32LE(cur + 0); // left
    }
    cur = stack.pop()!;
    const payloadStart = cur + RBTREE_OVERHEAD_BYTES;
    const payload = dynamic.subarray(payloadStart, payloadStart + (blockSize - RBTREE_OVERHEAD_BYTES));
    yield decode(payload, cur);
    cur = dynamic.readUInt32LE(cur + 4); // right
  }
}

/** Reverse in-order iteration (right-root-left). Yields nodes in descending
 *  `Ord` order. The on-chain matching engine always picks from the tree's
 *  `max_index` (rightmost) first, so reverse-in-order is "best-first" for
 *  both the bids and the asks tree — see the `Ord` impl on `RestingOrder`. */
export function* inOrderTraverseReverse<T>(
  dynamic: Buffer,
  rootIndex: number,
  blockSize: number,
  decode: RBPayloadDeserializer<T>,
): Generator<T> {
  if (rootIndex === NIL) return;
  const stack: number[] = [];
  let cur = rootIndex;
  while (cur !== NIL || stack.length > 0) {
    while (cur !== NIL) {
      stack.push(cur);
      cur = dynamic.readUInt32LE(cur + 4); // right
    }
    cur = stack.pop()!;
    const payloadStart = cur + RBTREE_OVERHEAD_BYTES;
    const payload = dynamic.subarray(payloadStart, payloadStart + (blockSize - RBTREE_OVERHEAD_BYTES));
    yield decode(payload, cur);
    cur = dynamic.readUInt32LE(cur + 0); // left
  }
}

/** Collect every payload in the tree into an array, in ascending `Ord` order. */
export function collectTree<T>(
  dynamic: Buffer,
  rootIndex: number,
  blockSize: number,
  decode: RBPayloadDeserializer<T>,
): T[] {
  return [...inOrderTraverse(dynamic, rootIndex, blockSize, decode)];
}

/** O(log N) descent: walk from root using `cmp(node) → -1|0|1` (negative
 *  ⇒ target < node ⇒ go left; positive ⇒ go right; zero ⇒ found). Returns
 *  `null` when no node satisfies `cmp == 0`. */
export function findInTree<T>(
  dynamic: Buffer,
  rootIndex: number,
  blockSize: number,
  decode: RBPayloadDeserializer<T>,
  cmp: (payload: Buffer, blockOffset: number) => number,
): T | null {
  if (rootIndex === NIL) return null;
  const payloadSize = blockSize - RBTREE_OVERHEAD_BYTES;
  let cur = rootIndex;
  while (cur !== NIL) {
    const payloadStart = cur + RBTREE_OVERHEAD_BYTES;
    const payload = dynamic.subarray(payloadStart, payloadStart + payloadSize);
    const c = cmp(payload, cur);
    if (c === 0) return decode(payload, cur);
    cur = c < 0 ? dynamic.readUInt32LE(cur + 0) : dynamic.readUInt32LE(cur + 4);
  }
  return null;
}
