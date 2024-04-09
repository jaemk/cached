/// This trait is used to solve the problem of the fact that the when we pass a key to the cache
/// we need to pass a key with no references so that borrowed data does not escape the function body.
/// In most cases this is just a case of calling [Clone::clone]
/// but for some types which contain references like [Option<&T>], the return of [Clone::clone] has references.
/// [ToFullyOwned::to_full_owned] is used within the proc_macro generated code.
#[doc(hidden)]
pub trait ToFullyOwned<Owned> {
    /// Returns the version of the type with no references.
    fn to_fully_owned(&self) -> Owned;
}

/// Generic implementation which covers cases where the is no type conversion.
impl<T> ToFullyOwned<T> for T
where
    // note Using T: Into<T> here so that we can don't have conflicting implementations.
    T: Into<T> + Clone,
{
    // type Owned = T;
    fn to_fully_owned(&self) -> T {
        self.clone()
    }
}

impl<T> ToFullyOwned<Option<T>> for Option<&T>
where
    T: Clone,
{
    fn to_fully_owned(&self) -> Option<T> {
        self.cloned()
    }
}

#[cfg(test)]
#[allow(dead_code)]
mod check_is_impl_for_expected_types {
    use super::*;

    fn check<T, U>()
    where
        T: ToFullyOwned<U>,
    {
    }

    fn compile_time_check() {
        check::<String, String>();
        check::<Option<String>, Option<String>>();
        check::<Option<&String>, Option<String>>();
        // check::<&String, String>();
    }
}

#[cfg(test)]
#[allow(dead_code)]
mod check_refs_can_be_converted_to_owned {
    use super::*;

    #[derive(Clone)]
    struct TestStruct;

    fn compile_time_check() {
        let s = &String::new();
        let _: String = s.to_fully_owned();

        let t = &TestStruct;
        let _: TestStruct = t.to_fully_owned();
    }
}
