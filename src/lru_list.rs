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

    pub fn iter(&self) -> LRUListIterator<'_, T> {
        LRUListIterator::<T> {
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
}
