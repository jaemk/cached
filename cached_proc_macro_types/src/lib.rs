/// Used to wrap a function result so callers can see whether the result was cached.
#[derive(Clone, Debug)]
pub struct Return<T> {
    was_cached: bool,
    value: T,
}

impl<T> Return<T> {
    pub fn new(value: T) -> Self {
        Self {
            was_cached: false,
            value,
        }
    }

    /// Returns `true` if the value came from the cache.
    pub fn was_cached(&self) -> bool {
        self.was_cached
    }

    /// Sets the `was_cached` flag. Used by generated macro code; not part of
    /// the supported public API.
    #[doc(hidden)]
    pub fn set_was_cached(&mut self, was_cached: bool) {
        self.was_cached = was_cached;
    }

    /// Consumes `self` and returns the inner value.
    pub fn into_inner(self) -> T {
        self.value
    }
}

impl<T> std::ops::Deref for Return<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T> std::ops::DerefMut for Return<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_was_cached_false() {
        let r = Return::new(42u32);
        assert!(!r.was_cached());
    }

    #[test]
    fn set_was_cached_updates_flag() {
        let mut r = Return::new(42u32);
        r.set_was_cached(true);
        assert!(r.was_cached());
    }

    #[test]
    fn into_inner_returns_owned_value() {
        let r = Return::new(String::from("hello"));
        let v = r.into_inner();
        assert_eq!(v, "hello");
    }

    #[test]
    fn deref_accesses_inner_value() {
        let r = Return::new(String::from("world"));
        assert_eq!(*r, "world");
        assert_eq!(r.to_uppercase(), "WORLD");
    }

    #[test]
    fn deref_mut_allows_mutation() {
        let mut r = Return::new(vec![1u32, 2, 3]);
        r.push(4);
        assert_eq!(*r, vec![1, 2, 3, 4]);
    }
}
