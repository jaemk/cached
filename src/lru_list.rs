/// Limited functionality doubly linked list using Vec as storage.
#[derive(Clone, Debug)]
pub struct LRUList<T> {
    values: Vec<ListEntry<T>>,
}

#[derive(Clone, Debug)]
struct ListEntry<T> {
    value: Option<T>,
    next: usize,
    prev: usize,
}

/// Free and occupied cells are each linked into a cyclic list with one auxiliary cell.
/// Cell #0 is on the list of free cells, element #1 is on the list of occupied cells.
///
impl<T> LRUList<T> {
    const FREE: usize = 0;
    const OCCUPIED: usize = 1;

    pub(crate) fn with_capacity(capacity: usize) -> LRUList<T> {
        let cap = capacity.saturating_add(2);
        let mut values = Vec::with_capacity(cap);
        values.push(ListEntry::<T> {
            value: None,
            next: 0,
            prev: 0,
        });
        values.push(ListEntry::<T> {
            value: None,
            next: 1,
            prev: 1,
        });
        LRUList { values }
    }

    pub(crate) fn try_with_capacity(
        capacity: usize,
    ) -> Result<LRUList<T>, crate::stores::BuildError> {
        let capacity = capacity
            .checked_add(2)
            .ok_or(crate::stores::BuildError::InvalidValue {
                field: "max_size",
                reason: "capacity overflow",
            })?;
        let mut values = Vec::new();
        values.try_reserve_exact(capacity).map_err(|_| {
            crate::stores::BuildError::InvalidValue {
                field: "max_size",
                reason: "allocation failed",
            }
        })?;
        values.push(ListEntry::<T> {
            value: None,
            next: 0,
            prev: 0,
        });
        values.push(ListEntry::<T> {
            value: None,
            next: 1,
            prev: 1,
        });
        Ok(LRUList { values })
    }

    pub(crate) fn unlink(&mut self, index: usize) {
        let prev = self.values[index].prev;
        let next = self.values[index].next;
        self.values[prev].next = next;
        self.values[next].prev = prev;
    }

    pub(crate) fn link_after(&mut self, index: usize, prev: usize) {
        let next = self.values[prev].next;
        self.values[index].prev = prev;
        self.values[index].next = next;
        self.values[prev].next = index;
        self.values[next].prev = index;
    }

    pub(crate) fn move_to_front(&mut self, index: usize) {
        self.unlink(index);
        self.link_after(index, Self::OCCUPIED);
    }

    pub(crate) fn push_front(&mut self, value: T) -> usize {
        if self.values[Self::FREE].next == Self::FREE {
            self.values.push(ListEntry::<T> {
                value: None,
                next: Self::FREE,
                prev: Self::FREE,
            });
            self.values[Self::FREE].next = self.values.len() - 1;
        }
        let index = self.values[Self::FREE].next;
        self.values[index].value = Some(value);
        self.unlink(index);
        self.link_after(index, Self::OCCUPIED);
        index
    }

    pub(crate) fn remove(&mut self, index: usize) -> T {
        self.unlink(index);
        self.link_after(index, Self::FREE);
        self.values[index].value.take().expect("invalid index")
    }

    pub(crate) fn back(&self) -> usize {
        self.values[Self::OCCUPIED].prev
    }

    pub(crate) fn get(&self, index: usize) -> &T {
        self.values[index].value.as_ref().expect("invalid index")
    }

    pub(crate) fn get_mut(&mut self, index: usize) -> &mut T {
        self.values[index].value.as_mut().expect("invalid index")
    }

    pub(crate) fn set(&mut self, index: usize, value: T) -> Option<T> {
        self.values[index].value.replace(value)
    }

    pub(crate) fn clear(&mut self) {
        self.values.clear();
        self.values.push(ListEntry::<T> {
            value: None,
            next: 0,
            prev: 0,
        });
        self.values.push(ListEntry::<T> {
            value: None,
            next: 1,
            prev: 1,
        });
    }

    /// Move every occupied value into `out` in MRU -> LRU order (leaving the list
    /// empty), then reset the two sentinel cells so the list is immediately reusable.
    ///
    /// This is the allocation-free counterpart of "collect the keys, then remove them
    /// one at a time": it walks the occupied chain once taking owned values, so callers
    /// clearing a whole cache never clone a key or re-hash anything. The backing `Vec`'s
    /// capacity is retained.
    pub(crate) fn drain_into(&mut self, out: &mut Vec<T>) {
        let mut index = self.values[Self::OCCUPIED].next;
        while index != Self::OCCUPIED {
            let next = self.values[index].next;
            if let Some(value) = self.values[index].value.take() {
                out.push(value);
            }
            index = next;
        }
        // Reset the free/occupied sentinels; every cell is now vacant.
        self.clear();
    }

    pub fn iter(&self) -> LRUListIterator<'_, T> {
        LRUListIterator::<T> {
            list: self,
            index: Self::OCCUPIED,
        }
    }

    /// Iterate the *slot indices* of the occupied cells in MRU -> LRU order (the same
    /// order as [`iter`](Self::iter)).
    ///
    /// Lets a sweep collect a `Vec<usize>` of the slots it intends to touch instead of
    /// cloning every candidate key. Slot indices are stable across removals of *other*
    /// slots, so a collected list stays valid while the sweep removes entries -- but a
    /// removal frees its slot for reuse, so a collected index must not be replayed after
    /// any `push_front`.
    pub(crate) fn iter_indices(&self) -> LRUListIndexIterator<'_, T> {
        LRUListIndexIterator::<T> {
            list: self,
            index: Self::OCCUPIED,
        }
    }
}

#[derive(Debug)]
pub struct LRUListIterator<'a, T> {
    list: &'a LRUList<T>,
    index: usize,
}

impl<'a, T> Iterator for LRUListIterator<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.list.values[self.index].next;
        if next == LRUList::<T>::OCCUPIED {
            None
        } else {
            let value = self.list.values[next].value.as_ref();
            self.index = next;
            value
        }
    }
}

/// Iterator over the occupied slot indices of an [`LRUList`], MRU -> LRU.
/// See [`LRUList::iter_indices`].
#[derive(Debug)]
pub struct LRUListIndexIterator<'a, T> {
    list: &'a LRUList<T>,
    index: usize,
}

impl<T> Iterator for LRUListIndexIterator<'_, T> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.list.values[self.index].next;
        if next == LRUList::<T>::OCCUPIED {
            None
        } else {
            self.index = next;
            Some(next)
        }
    }
}

#[cfg(test)]
mod tests {
    // Direct coverage of the slab/free-list invariants that `LruCache`,
    // `LruTtlCache`, and `ExpiringLruCache` rely on (index stability across
    // unrelated removals; freed-slot reuse; MRU/LRU ordering). Previously only
    // exercised indirectly via the store tests.
    use super::LRUList;

    fn order(l: &LRUList<i32>) -> Vec<i32> {
        l.iter().copied().collect()
    }

    #[test]
    fn push_order_and_back() {
        let mut l = LRUList::with_capacity(4);
        assert!(order(&l).is_empty());
        let a = l.push_front(1);
        let b = l.push_front(2);
        let c = l.push_front(3);
        assert_eq!(order(&l), vec![3, 2, 1]); // MRU -> LRU
        assert_eq!(*l.get(a), 1);
        assert_eq!(*l.get(b), 2);
        assert_eq!(*l.get(c), 3);
        assert_eq!(l.back(), a); // oldest
    }

    #[test]
    fn index_stable_across_other_removal() {
        let mut l = LRUList::with_capacity(4);
        let a = l.push_front(10);
        let b = l.push_front(20);
        let c = l.push_front(30);
        assert_eq!(l.remove(b), 20);
        // a and c indices must remain valid after removing an unrelated node.
        assert_eq!(*l.get(a), 10);
        assert_eq!(*l.get(c), 30);
        assert_eq!(order(&l), vec![30, 10]);
    }

    #[test]
    fn freed_slots_are_reused() {
        let mut l = LRUList::with_capacity(2);
        let a = l.push_front(1);
        assert_eq!(l.remove(a), 1);
        let b = l.push_front(2);
        assert_eq!(a, b, "a freed slot must be reused, not grown");
        assert_eq!(*l.get(b), 2);
        assert_eq!(order(&l), vec![2]);
    }

    #[test]
    fn move_to_front_reorders() {
        let mut l = LRUList::with_capacity(4);
        let a = l.push_front(1);
        let b = l.push_front(2);
        let _c = l.push_front(3);
        assert_eq!(order(&l), vec![3, 2, 1]);
        l.move_to_front(a);
        assert_eq!(order(&l), vec![1, 3, 2]);
        assert_eq!(l.back(), b); // 2 is now LRU
    }

    /// Walk the occupied ring backwards via `prev`. `order` only follows `next`, so a
    /// corrupted `prev` chain is invisible to it; comparing the two directions is what
    /// actually proves the doubly-linked list is intact.
    fn order_reversed(l: &LRUList<i32>) -> Vec<i32> {
        let mut out = Vec::new();
        let mut idx = l.values[LRUList::<i32>::OCCUPIED].prev;
        while idx != LRUList::<i32>::OCCUPIED {
            out.push(*l.get(idx));
            idx = l.values[idx].prev;
        }
        out.reverse();
        out
    }

    /// `move_to_front` on an index that is ALREADY the head is on the hot path now that
    /// `cache_set` promotes an overwritten key. It aliases (`unlink` writes the same
    /// sentinel links `link_after` then re-reads), so it gets its own integrity check
    /// rather than only a shallow forward-order assertion.
    #[test]
    fn move_to_front_on_the_current_head_keeps_both_link_directions_intact() {
        let mut l = LRUList::with_capacity(4);
        let a = l.push_front(1);
        let _b = l.push_front(2);
        let c = l.push_front(3);
        assert_eq!(order(&l), vec![3, 2, 1]);

        // `c` is already the head; promoting it must be a no-op in both directions.
        for _ in 0..3 {
            l.move_to_front(c);
            assert_eq!(order(&l), vec![3, 2, 1]);
            assert_eq!(order_reversed(&l), order(&l), "prev chain must mirror next");
            assert_eq!(l.back(), a);
        }

        // And the list still behaves normally afterwards.
        l.move_to_front(a);
        assert_eq!(order(&l), vec![1, 3, 2]);
        assert_eq!(order_reversed(&l), order(&l));
    }

    /// The degenerate case: promoting the only entry empties and rebuilds the ring.
    #[test]
    fn move_to_front_on_the_sole_entry_keeps_the_ring_intact() {
        let mut l = LRUList::with_capacity(2);
        let a = l.push_front(42);
        for _ in 0..3 {
            l.move_to_front(a);
            assert_eq!(order(&l), vec![42]);
            assert_eq!(order_reversed(&l), vec![42]);
            assert_eq!(l.back(), a);
        }
        // A later push must still link correctly onto the rebuilt ring.
        let b = l.push_front(7);
        assert_eq!(order(&l), vec![7, 42]);
        assert_eq!(order_reversed(&l), order(&l));
        assert_eq!(l.back(), a);
        assert_eq!(*l.get(b), 7);
    }

    #[test]
    fn set_replaces_and_clear_resets() {
        let mut l = LRUList::with_capacity(2);
        let a = l.push_front(7);
        assert_eq!(l.set(a, 8), Some(7));
        assert_eq!(*l.get(a), 8);
        l.clear();
        assert!(order(&l).is_empty());
        let b = l.push_front(9); // still usable after clear
        assert_eq!(*l.get(b), 9);
    }

    #[test]
    fn iter_indices_matches_iter_order() {
        let mut l = LRUList::with_capacity(4);
        let a = l.push_front(1);
        let b = l.push_front(2);
        let c = l.push_front(3);
        assert_eq!(l.iter_indices().collect::<Vec<_>>(), vec![c, b, a]);
        // The index order must track the value order (MRU -> LRU) exactly.
        let by_index: Vec<i32> = l.iter_indices().map(|i| *l.get(i)).collect();
        assert_eq!(by_index, order(&l));

        // Reordering and removal keep the two views in agreement.
        l.move_to_front(a);
        assert_eq!(l.iter_indices().collect::<Vec<_>>(), vec![a, c, b]);
        assert_eq!(l.remove(c), 3);
        assert_eq!(l.iter_indices().collect::<Vec<_>>(), vec![a, b]);
        let by_index: Vec<i32> = l.iter_indices().map(|i| *l.get(i)).collect();
        assert_eq!(by_index, order(&l));

        // Empty list yields no indices.
        let empty: LRUList<i32> = LRUList::with_capacity(2);
        assert!(empty.iter_indices().next().is_none());
    }

    #[test]
    fn drain_into_yields_mru_to_lru_and_resets() {
        let mut l = LRUList::with_capacity(4);
        l.push_front(1);
        l.push_front(2);
        let c = l.push_front(3);
        l.move_to_front(c); // no-op, but pins that drain follows the live chain

        let mut out = Vec::new();
        l.drain_into(&mut out);
        assert_eq!(out, vec![3, 2, 1], "drain must be MRU -> LRU");

        // Sentinels are reset: the list is empty and reports no indices.
        assert!(order(&l).is_empty());
        assert!(l.iter_indices().next().is_none());

        // Reusable after a drain, and slot allocation restarts from the free list.
        let a = l.push_front(9);
        let b = l.push_front(10);
        assert_eq!(*l.get(a), 9);
        assert_eq!(*l.get(b), 10);
        assert_eq!(order(&l), vec![10, 9]);
        assert_eq!(l.back(), a);

        // Freed-slot reuse still works after a drain.
        assert_eq!(l.remove(b), 10);
        let d = l.push_front(11);
        assert_eq!(d, b, "a freed slot must be reused after a drain, not grown");
        assert_eq!(order(&l), vec![11, 9]);

        // Draining an already-empty list appends nothing and leaves it usable.
        let mut l2: LRUList<i32> = LRUList::with_capacity(2);
        let mut out2 = vec![42];
        l2.drain_into(&mut out2);
        assert_eq!(out2, vec![42]);
        let e = l2.push_front(5);
        assert_eq!(*l2.get(e), 5);
    }

    #[test]
    fn stale_index_after_push_front_refers_to_recycled_slot() {
        // Pins the hazard documented on `iter_indices`: a collected index survives the
        // removal of *other* slots, but a `push_front` recycles the freed slot, so
        // replaying a stale index afterwards addresses the NEW occupant. Consumers that
        // collect indices must not insert before they finish replaying them.
        let mut l = LRUList::with_capacity(4);
        let a = l.push_front(1);
        let b = l.push_front(2);
        let c = l.push_front(3);
        let snapshot: Vec<usize> = l.iter_indices().collect();
        assert_eq!(snapshot, vec![c, b, a]);

        assert_eq!(l.remove(b), 2);
        // Supported case: indices of untouched slots are still valid after a removal.
        assert_eq!(*l.get(snapshot[0]), 3);
        assert_eq!(*l.get(snapshot[2]), 1);

        // A push recycles the just-freed slot ...
        let d = l.push_front(99);
        assert_eq!(d, b, "push_front must recycle the most recently freed slot");
        // ... so the stale index now names the new entry, silently and without a panic.
        assert_eq!(*l.get(snapshot[1]), 99);
        assert_eq!(
            l.remove(snapshot[1]),
            99,
            "replaying a stale index removes the recycled entry, not the original"
        );
        assert_eq!(order(&l), vec![3, 1]);
    }

    #[test]
    fn iter_indices_empty_after_all_removals() {
        let mut l = LRUList::with_capacity(4);
        let a = l.push_front(1);
        let b = l.push_front(2);
        assert_eq!(l.remove(a), 1);
        assert_eq!(l.remove(b), 2);
        assert!(l.iter_indices().next().is_none());
        assert!(order(&l).is_empty());
    }

    #[test]
    #[should_panic(expected = "invalid index")]
    fn remove_of_freed_slot_panics() {
        // `LruCache::remove_index` documents a panic for a non-occupied slot; this is
        // the primitive it panics through.
        let mut l = LRUList::with_capacity(2);
        let a = l.push_front(1);
        assert_eq!(l.remove(a), 1);
        let _ = l.remove(a);
    }

    #[test]
    #[should_panic(expected = "invalid index")]
    fn get_of_freed_slot_panics() {
        let mut l = LRUList::with_capacity(2);
        let a = l.push_front(1);
        assert_eq!(l.remove(a), 1);
        let _ = l.get(a);
    }

    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn get_out_of_range_index_panics() {
        let mut l = LRUList::with_capacity(2);
        l.push_front(1);
        let _ = l.get(999);
    }

    #[test]
    fn drain_into_skips_freed_slots() {
        // A list with holes (removed entries) must drain only the live chain.
        let mut l = LRUList::with_capacity(8);
        let a = l.push_front(1);
        let b = l.push_front(2);
        let _c = l.push_front(3);
        let d = l.push_front(4);
        assert_eq!(l.remove(b), 2);
        assert_eq!(l.remove(d), 4);
        l.move_to_front(a);

        let mut out = Vec::new();
        l.drain_into(&mut out);
        assert_eq!(out, vec![1, 3]);
        assert!(order(&l).is_empty());
    }
}
