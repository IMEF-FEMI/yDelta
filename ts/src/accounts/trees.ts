/**
 * Generic hypertree walkers over a `bytemuck` red-black tree embedded in
 * an account's dynamic region.
 *
 * `DataIndex` is a byte offset (NOT a node ordinal). Every node is laid out as:
 *
 *   @0   u32   left          (NIL = no child)
 *   @4   u32   right         (NIL = no child)
 *   @8   u32   parent        (NIL = root)
 *   @12  u8    color         (0 = Black, 1 = Red)
 *   @13  u8    payload_type
 *   @14  u16   _padding
 *   @16  V     payload       (length depends on tree type)
 *
 * The on-chain `iter()` walks from `max_index` via `get_next_lower_index`, i.e.
 * in **descending key order**. We mirror that — `walkDescending(...)` matches
 * the on-chain trait. Callers that want ascending order can `.reverse()` or
 * use `walkAscending(...)`.
 */
import { NIL_INDEX, isNil, readU32 } from './_read.js';

export const NODE_HEADER_BYTES = 16;

const LEFT_OFF = 0;
const RIGHT_OFF = 4;
const PARENT_OFF = 8;

function leftIndex(dynamic: DataView, index: number): number {
  return readU32(dynamic, index + LEFT_OFF);
}
function rightIndex(dynamic: DataView, index: number): number {
  return readU32(dynamic, index + RIGHT_OFF);
}
function parentIndex(dynamic: DataView, index: number): number {
  return readU32(dynamic, index + PARENT_OFF);
}

/** Find the smallest-key node in the subtree rooted at `start`. */
function leftmost(dynamic: DataView, start: number): number {
  let cur = start;
  while (!isNil(cur)) {
    const left = leftIndex(dynamic, cur);
    if (isNil(left)) return cur;
    cur = left;
  }
  return NIL_INDEX;
}

/** Find the largest-key node in the subtree rooted at `start`. */
function rightmost(dynamic: DataView, start: number): number {
  let cur = start;
  while (!isNil(cur)) {
    const right = rightIndex(dynamic, cur);
    if (isNil(right)) return cur;
    cur = right;
  }
  return NIL_INDEX;
}

/** In-order predecessor of `index`. NIL when `index` is the min. */
function predecessor(dynamic: DataView, index: number): number {
  if (isNil(index)) return NIL_INDEX;
  const left = leftIndex(dynamic, index);
  if (!isNil(left)) return rightmost(dynamic, left);
  // Walk up while we are a left child.
  let cur = index;
  let parent = parentIndex(dynamic, cur);
  while (!isNil(parent) && leftIndex(dynamic, parent) === cur) {
    cur = parent;
    parent = parentIndex(dynamic, cur);
  }
  return parent;
}

/** In-order successor of `index`. NIL when `index` is the max. */
function successor(dynamic: DataView, index: number): number {
  if (isNil(index)) return NIL_INDEX;
  const right = rightIndex(dynamic, index);
  if (!isNil(right)) return leftmost(dynamic, right);
  let cur = index;
  let parent = parentIndex(dynamic, cur);
  while (!isNil(parent) && rightIndex(dynamic, parent) === cur) {
    cur = parent;
    parent = parentIndex(dynamic, cur);
  }
  return parent;
}

/**
 * Walk an RB-tree in descending key order — the same order as the on-chain
 * `HyperTreeValueIteratorTrait::iter()`. Yields `(index, payloadView)` pairs;
 * `payloadView` is a sub-`DataView` over the node's payload region (NOT the
 * full node), so the caller can read fields directly.
 */
export function* walkDescending(
  dynamic: DataView,
  rootIndex: number,
  payloadSize: number,
): Generator<{ index: number; payload: DataView }> {
  if (isNil(rootIndex)) return;
  let cur = rightmost(dynamic, rootIndex);
  while (!isNil(cur)) {
    const payloadOff = cur + NODE_HEADER_BYTES;
    yield {
      index: cur,
      payload: new DataView(dynamic.buffer, dynamic.byteOffset + payloadOff, payloadSize),
    };
    cur = predecessor(dynamic, cur);
  }
}

/** Walk in ascending key order (smallest → largest). */
export function* walkAscending(
  dynamic: DataView,
  rootIndex: number,
  payloadSize: number,
): Generator<{ index: number; payload: DataView }> {
  if (isNil(rootIndex)) return;
  let cur = leftmost(dynamic, rootIndex);
  while (!isNil(cur)) {
    const payloadOff = cur + NODE_HEADER_BYTES;
    yield {
      index: cur,
      payload: new DataView(dynamic.buffer, dynamic.byteOffset + payloadOff, payloadSize),
    };
    cur = successor(dynamic, cur);
  }
}

/**
 * Walk a hypertree free list — single-linked at +0 of each free block.
 * Stops at NIL. Useful for diagnostics / allocator inspection.
 */
export function* walkFreeList(
  dynamic: DataView,
  headIndex: number,
): Generator<number> {
  let cur = headIndex;
  while (!isNil(cur)) {
    yield cur;
    cur = readU32(dynamic, cur);
  }
}
