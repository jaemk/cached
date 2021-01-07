use std::cmp::Eq;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::hash::Hash;

use super::Cached;

#[cfg(feature = "async")]
use {super::CachedAsync, async_trait::async_trait, futures::Future};

pub mod sized;
pub mod timed;
pub mod timed_sized;
pub mod unbound;

pub type UnboundCache<K, V> = unbound::UnboundCache<K, V>;
pub type SizedCache<K, V> = sized::SizedCache<K, V>;
pub type TimedSizedCache<K, V> = timed_sized::TimedSizedCache<K, V>;
pub type TimedCache<K, V> = timed::TimedCache<K, V>;

/// Limited functionality doubly linked list using Vec as storage.
#[derive(Clone, Debug)]
struct LRUList<T> {
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

    fn with_capacity(capacity: usize) -> LRUList<T> {
        let mut values = Vec::with_capacity(capacity + 2);
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

    fn unlink(&mut self, index: usize) {
        let prev = self.values[index].prev;
        let next = self.values[index].next;
        self.values[prev].next = next;
        self.values[next].prev = prev;
    }

    fn link_after(&mut self, index: usize, prev: usize) {
        let next = self.values[prev].next;
        self.values[index].prev = prev;
        self.values[index].next = next;
        self.values[prev].next = index;
        self.values[next].prev = index;
    }

    fn move_to_front(&mut self, index: usize) {
        self.unlink(index);
        self.link_after(index, Self::OCCUPIED);
    }

    fn push_front(&mut self, value: T) -> usize {
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

    fn remove(&mut self, index: usize) -> T {
        self.unlink(index);
        self.link_after(index, Self::FREE);
        self.values[index].value.take().expect("invalid index")
    }

    fn back(&self) -> usize {
        self.values[Self::OCCUPIED].prev
    }

    fn get(&self, index: usize) -> &T {
        self.values[index].value.as_ref().expect("invalid index")
    }

    fn get_mut(&mut self, index: usize) -> &mut T {
        self.values[index].value.as_mut().expect("invalid index")
    }

    fn set(&mut self, index: usize, value: T) -> Option<T> {
        self.values[index].value.replace(value)
    }

    fn clear(&mut self) {
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

    fn iter(&self) -> LRUListIterator<T> {
        LRUListIterator::<T> {
            list: self,
            index: Self::OCCUPIED,
        }
    }
}

#[derive(Debug)]
struct LRUListIterator<'a, T> {
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

impl<K: Hash + Eq, V> Cached<K, V> for HashMap<K, V> {
    fn cache_get(&mut self, k: &K) -> Option<&V> {
        self.get(k)
    }
    fn cache_get_mut(&mut self, k: &K) -> Option<&mut V> {
        self.get_mut(k)
    }
    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        self.entry(key).or_insert_with(f)
    }
    fn cache_set(&mut self, k: K, v: V) -> Option<V> {
        self.insert(k, v)
    }
    fn cache_remove(&mut self, k: &K) -> Option<V> {
        self.remove(k)
    }
    fn cache_clear(&mut self) {
        self.clear();
    }
    fn cache_reset(&mut self) {
        *self = HashMap::new();
    }
    fn cache_size(&self) -> usize {
        self.len()
    }
}

#[cfg(feature = "async")]
#[async_trait]
impl<K, V> CachedAsync<K, V> for HashMap<K, V>
where
    K: Hash + Eq + Clone + Send,
{
    async fn get_or_set_with<F, Fut>(&mut self, k: K, f: F) -> &mut V
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = V> + Send,
    {
        match self.entry(k) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => v.insert(f().await),
        }
    }

    async fn try_get_or_set_with<F, Fut, E>(&mut self, k: K, f: F) -> Result<&mut V, E>
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<V, E>> + Send,
    {
        let v = match self.entry(k) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => v.insert(f().await?),
        };

        Ok(v)
    }
}

#[cfg(test)]
/// Cache store tests
mod tests {
    use super::*;

    #[test]
    fn hashmap() {
        let mut c = std::collections::HashMap::new();
        assert!(c.cache_get(&1).is_none());
        assert_eq!(c.cache_misses(), None);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_get(&1), Some(&100));
        assert_eq!(c.cache_hits(), None);
        assert_eq!(c.cache_misses(), None);
    }
}
