import { expect } from 'chai';
import { collectTree, inOrderTraverse, inOrderTraverseReverse, findInTree } from '../src/utils/redBlackTree';
import { NIL, RBTREE_OVERHEAD_BYTES } from '../src/utils/discriminator';

/** Build a dynamic-region buffer with explicit BST nodes laid out as
 *  contiguous fixed-size blocks. Each block: 16-byte RB header + payload.
 *
 *  Returns the buffer and the byte-offset of `rootIndex`. */
function buildTree(
  blockSize: number,
  /** Linked-tuple description of nodes:
   *  `{ offset, left, right, parent, payload }`. */
  nodes: { offset: number; left: number; right: number; parent: number; payload: Buffer }[],
): Buffer {
  const totalBytes = Math.max(...nodes.map((n) => n.offset)) + blockSize;
  const buf = Buffer.alloc(totalBytes);
  for (const n of nodes) {
    buf.writeUInt32LE(n.left, n.offset + 0);
    buf.writeUInt32LE(n.right, n.offset + 4);
    buf.writeUInt32LE(n.parent, n.offset + 8);
    buf.writeUInt8(0, n.offset + 12); // color
    buf.writeUInt8(0, n.offset + 13); // payload_type
    buf.writeUInt16LE(0, n.offset + 14);
    n.payload.copy(buf, n.offset + RBTREE_OVERHEAD_BYTES);
  }
  return buf;
}

describe('redBlackTree in-order traversal', () => {
  const BLOCK = 32;
  const PAYLOAD = BLOCK - RBTREE_OVERHEAD_BYTES;

  it('returns empty for NIL root', () => {
    const out = collectTree(Buffer.alloc(0), NIL, BLOCK, (p) => p[0]);
    expect(out).to.deep.equal([]);
  });

  it('yields a single node tree', () => {
    const payload = Buffer.alloc(PAYLOAD, 42);
    const buf = buildTree(BLOCK, [{ offset: 0, left: NIL, right: NIL, parent: NIL, payload }]);
    const out = collectTree(buf, 0, BLOCK, (p) => p[0]);
    expect(out).to.deep.equal([42]);
  });

  it('yields nodes in in-order (sorted by tree position, not byte offset)', () => {
    // Tree:        20
    //             /  \
    //            10  30
    // Layout in dynamic region (any order works):
    //   offset 0:  20 (root), left=BLOCK, right=2*BLOCK
    //   offset BLOCK: 10 (left child)
    //   offset 2*BLOCK: 30 (right child)
    const root = 0;
    const leftOff = BLOCK;
    const rightOff = 2 * BLOCK;
    const buf = buildTree(BLOCK, [
      { offset: root, left: leftOff, right: rightOff, parent: NIL, payload: Buffer.from([20, ...new Array(PAYLOAD - 1).fill(0)]) },
      { offset: leftOff, left: NIL, right: NIL, parent: root, payload: Buffer.from([10, ...new Array(PAYLOAD - 1).fill(0)]) },
      { offset: rightOff, left: NIL, right: NIL, parent: root, payload: Buffer.from([30, ...new Array(PAYLOAD - 1).fill(0)]) },
    ]);
    const out = collectTree(buf, root, BLOCK, (p) => p[0]);
    expect(out).to.deep.equal([10, 20, 30]);
  });

  it('generator form supports early break', () => {
    const root = 0;
    const buf = buildTree(BLOCK, [
      { offset: root, left: BLOCK, right: 2 * BLOCK, parent: NIL, payload: Buffer.from([20, ...new Array(PAYLOAD - 1).fill(0)]) },
      { offset: BLOCK, left: NIL, right: NIL, parent: root, payload: Buffer.from([10, ...new Array(PAYLOAD - 1).fill(0)]) },
      { offset: 2 * BLOCK, left: NIL, right: NIL, parent: root, payload: Buffer.from([30, ...new Array(PAYLOAD - 1).fill(0)]) },
    ]);
    const seen: number[] = [];
    for (const v of inOrderTraverse(buf, root, BLOCK, (p) => p[0])) {
      seen.push(v);
      if (seen.length === 2) break;
    }
    expect(seen).to.deep.equal([10, 20]);
  });

  it('reverse in-order visits right-root-left (descending Ord)', () => {
    // Same tree:        20
    //                  /  \
    //                 10  30
    // Reverse in-order should yield [30, 20, 10] — best-first when the
    // tree's max_index is the "best" element (true for both bids and asks
    // per the matching-engine convention).
    const root = 0;
    const buf = buildTree(BLOCK, [
      { offset: root, left: BLOCK, right: 2 * BLOCK, parent: NIL, payload: Buffer.from([20, ...new Array(PAYLOAD - 1).fill(0)]) },
      { offset: BLOCK, left: NIL, right: NIL, parent: root, payload: Buffer.from([10, ...new Array(PAYLOAD - 1).fill(0)]) },
      { offset: 2 * BLOCK, left: NIL, right: NIL, parent: root, payload: Buffer.from([30, ...new Array(PAYLOAD - 1).fill(0)]) },
    ]);
    const out = [...inOrderTraverseReverse(buf, root, BLOCK, (p) => p[0])];
    expect(out).to.deep.equal([30, 20, 10]);
  });

  it('findInTree descends by comparator without walking the full tree', () => {
    const root = 0;
    const buf = buildTree(BLOCK, [
      { offset: root, left: BLOCK, right: 2 * BLOCK, parent: NIL, payload: Buffer.from([20, ...new Array(PAYLOAD - 1).fill(0)]) },
      { offset: BLOCK, left: NIL, right: NIL, parent: root, payload: Buffer.from([10, ...new Array(PAYLOAD - 1).fill(0)]) },
      { offset: 2 * BLOCK, left: NIL, right: NIL, parent: root, payload: Buffer.from([30, ...new Array(PAYLOAD - 1).fill(0)]) },
    ]);
    const hit = findInTree(buf, root, BLOCK, (p) => p[0], (p) => 30 - p[0]);
    expect(hit).to.equal(30);
    const miss = findInTree(buf, root, BLOCK, (p) => p[0], (p) => 99 - p[0]);
    expect(miss).to.equal(null);
  });
});
