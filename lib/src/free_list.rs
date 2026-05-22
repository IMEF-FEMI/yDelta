use bytemuck::{Pod, Zeroable};

#[cfg(debug_assertions)]
use crate::get_helper;
use crate::{get_mut_helper, DataIndex, Get, NIL};

// FreeList is a linked list that keeps track of all the available nodes that
// can be filled with ClaimedSeats and RestingOrders.
const END: u32 = u32::MAX;

/// Upper bound on how far [`FreeList::add`]'s double-free scan will walk
/// the list before giving up. The free list can never legitimately be
/// longer than the number of blocks in the largest account (~10 MiB /
/// smallest block), so this is comfortably above any real list length
/// while still bounding the worst-case cost of the guard.
#[cfg(debug_assertions)]
const MAX_FREE_LIST_SCAN: usize = 1 << 20;
pub struct FreeList<'a, T: Pod> {
    /// Index in data of the head of the free list.
    head_index: DataIndex,
    /// Mutable data array of bytes in which the free list lives.
    data: &'a mut [u8],

    /// Placeholder for holding the data type.
    phantom: std::marker::PhantomData<&'a T>,
}

#[derive(Default, Copy, Clone, Zeroable)]
#[repr(C)]
pub struct FreeListNode<T> {
    /// Next in the linked list.
    next_index: DataIndex,
    /// Payload. For free lists, this is just unused, zeroed bytes.
    node_inner: T,
}
unsafe impl<T: Pod> Pod for FreeListNode<T> {}
impl<T: Pod> Get for FreeListNode<T> {}
impl<T: Pod> FreeListNode<T> {
    pub fn has_next(&self) -> bool {
        self.next_index != NIL
    }

    pub fn get_next_index(&self) -> DataIndex {
        self.next_index
    }
}

impl<'a, T: Pod> FreeList<'a, T> {
    /// Create a new free list. Assumes that the data within data is already a well
    /// formed FreeList.
    pub fn new(data: &'a mut [u8], head_index: DataIndex) -> Self {
        FreeList {
            head_index,
            data,
            phantom: std::marker::PhantomData,
        }
    }

    /// Gets the index of head.
    pub fn get_head(&self) -> DataIndex {
        self.head_index
    }

    /// Free a node to the free list.
    ///
    /// Double-free guard (debug builds only): re-linking an index
    /// that is already on the free list cycles the list, and a later
    /// `remove()` then hands the same slot out twice — two live nodes
    /// aliasing one block, silently corrupting whichever trees they
    /// belong to. A full scan on every free is O(n) per tree-node
    /// removal, so it is gated to debug/test builds; release builds rely
    /// on the program layer proving single-free. The scan is bounded by
    /// `MAX_FREE_LIST_SCAN` so a pre-corrupted cyclic list cannot hang.
    pub fn add(&mut self, index: DataIndex) {
        #[cfg(debug_assertions)]
        {
            let mut cursor: DataIndex = self.head_index;
            let mut steps: usize = 0;
            while cursor != END && steps < MAX_FREE_LIST_SCAN {
                assert_ne!(cursor, index, "FreeList::add: double-free of index {index}");
                cursor = get_helper::<FreeListNode<T>>(self.data, cursor).next_index;
                steps += 1;
            }
        }
        let node: &mut FreeListNode<T> = get_mut_helper::<FreeListNode<T>>(self.data, index);
        node.node_inner = T::zeroed();
        node.next_index = self.head_index;
        self.head_index = index;
    }

    /// Free the node at index
    pub fn remove(&mut self) -> DataIndex {
        if self.head_index == END {
            return END;
        }

        let free_node_index: DataIndex = self.head_index;
        let head: &mut FreeListNode<T> =
            get_mut_helper::<FreeListNode<T>>(self.data, free_node_index);

        self.head_index = head.next_index;

        // Do not need to zero the bytes because it was zeroes when adding to
        // the free list.
        head.next_index = 0;

        free_node_index
    }
}

mod test {
    use super::*;

    #[allow(unused)]
    #[repr(C, packed)]
    #[derive(Default, Copy, Clone, Pod, Zeroable)]
    struct UnusedFreeListPadding1 {
        _padding: [u8; 1],
    }

    #[test]
    fn test_free_list_basic() {
        let mut data: [u8; 100000] = [0; 100000];
        let mut free_list: FreeList<UnusedFreeListPadding1> = FreeList::new(&mut data, END);
        free_list.add(64);
        free_list.add(128);

        assert_eq!(128, free_list.remove());
        assert_eq!(64, free_list.remove());
        assert_eq!(END, free_list.remove());
    }
}
