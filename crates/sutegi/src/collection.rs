//! `Collection<T>` — a fluent wrapper over an owned `Vec<T>`, giving Rust
//! iterables a chainable, expressive API for the everyday shaping that
//! `Iterator` makes verbose (`pluck`, `group_by`, `partition`, `chunk`,
//! `implode`, …).
//!
//! It is thin on purpose: a `Collection<T>` *is* a `Vec<T>` with methods, so it
//! `Deref`s to `[T]`, round-trips through `Vec`/iterators for free, and adds
//! **zero** allocation over doing the same work by hand. No third-party deps —
//! just `std`.
//!
//! ```
//! use sutegi::collect;
//!
//! let names = collect(vec![1, 2, 3, 4, 5, 6])
//!     .filter(|n| n % 2 == 0)     // 2, 4, 6
//!     .map(|n| n * 10)            // 20, 40, 60
//!     .reverse()
//!     .implode(", ");             // "60, 40, 20"
//!
//! assert_eq!(names, "60, 40, 20");
//! ```

use std::collections::HashMap;
use std::hash::Hash;

/// Wrap any iterable in a [`Collection`] to start a fluent chain.
///
/// ```
/// use sutegi::collect;
/// let total: i64 = collect(vec![1, 2, 3]).map(|n| n * 2).sum();
/// assert_eq!(total, 12);
/// ```
pub fn collect<T, I: IntoIterator<Item = T>>(items: I) -> Collection<T> {
    Collection {
        items: items.into_iter().collect(),
    }
}

/// A fluent, owned collection of `T`. See the [module docs](self).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collection<T> {
    items: Vec<T>,
}

// Manual `Default` so an empty collection exists for *any* `T` — the derive
// would wrongly demand `T: Default`.
impl<T> Default for Collection<T> {
    fn default() -> Collection<T> {
        Collection::new()
    }
}

impl<T> Collection<T> {
    /// An empty collection.
    pub fn new() -> Collection<T> {
        Collection { items: Vec::new() }
    }

    // --- terminal accessors -------------------------------------------------

    /// Borrow the underlying items as a slice.
    pub fn all(&self) -> &[T] {
        &self.items
    }

    /// Consume the collection, returning the owned `Vec<T>`.
    pub fn into_vec(self) -> Vec<T> {
        self.items
    }

    /// Number of items.
    pub fn count(&self) -> usize {
        self.items.len()
    }

    /// `true` when there are no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// First item, if any.
    pub fn first(&self) -> Option<&T> {
        self.items.first()
    }

    /// Last item, if any.
    pub fn last(&self) -> Option<&T> {
        self.items.last()
    }

    /// The item at `index`, if in range.
    pub fn get(&self, index: usize) -> Option<&T> {
        self.items.get(index)
    }

    // --- transformations (consume + return a new collection) ----------------

    /// Map every item through `f`, producing a `Collection<U>`.
    pub fn map<U, F: FnMut(T) -> U>(self, f: F) -> Collection<U> {
        collect(self.items.into_iter().map(f))
    }

    /// Keep only items for which `pred` returns `true`.
    pub fn filter<F: FnMut(&T) -> bool>(self, mut pred: F) -> Collection<T> {
        collect(self.items.into_iter().filter(|x| pred(x)))
    }

    /// Drop items for which `pred` returns `true` — the inverse of [`filter`](Self::filter).
    pub fn reject<F: FnMut(&T) -> bool>(self, mut pred: F) -> Collection<T> {
        collect(self.items.into_iter().filter(|x| !pred(x)))
    }

    /// Map-and-filter in one pass: keep each `Some(u)` produced by `f`.
    pub fn filter_map<U, F: FnMut(T) -> Option<U>>(self, f: F) -> Collection<U> {
        collect(self.items.into_iter().filter_map(f))
    }

    /// Map each item to an iterator and flatten the results.
    pub fn flat_map<U, I, F>(self, f: F) -> Collection<U>
    where
        I: IntoIterator<Item = U>,
        F: FnMut(T) -> I,
    {
        collect(self.items.into_iter().flat_map(f))
    }

    /// Append another iterable's items to this one.
    pub fn concat<I: IntoIterator<Item = T>>(mut self, other: I) -> Collection<T> {
        self.items.extend(other);
        self
    }

    /// Push a single item onto the end.
    pub fn push(mut self, item: T) -> Collection<T> {
        self.items.push(item);
        self
    }

    /// Reverse the order of items.
    pub fn reverse(mut self) -> Collection<T> {
        self.items.reverse();
        self
    }

    /// Take the first `n` items (fewer if the collection is shorter).
    pub fn take(mut self, n: usize) -> Collection<T> {
        self.items.truncate(n);
        self
    }

    /// Skip the first `n` items.
    pub fn skip(self, n: usize) -> Collection<T> {
        collect(self.items.into_iter().skip(n))
    }

    /// Split into contiguous chunks of at most `size` items each.
    ///
    /// Panics if `size == 0`, matching [`slice::chunks`].
    pub fn chunk(self, size: usize) -> Collection<Collection<T>> {
        assert!(size != 0, "chunk size must be non-zero");
        let mut out: Vec<Collection<T>> = Vec::new();
        let mut current: Vec<T> = Vec::with_capacity(size);
        for item in self.items {
            current.push(item);
            if current.len() == size {
                out.push(Collection {
                    items: std::mem::take(&mut current),
                });
            }
        }
        if !current.is_empty() {
            out.push(Collection { items: current });
        }
        collect(out)
    }

    /// Split into two collections: `(matching, rest)` per `pred`.
    pub fn partition<F: FnMut(&T) -> bool>(self, mut pred: F) -> (Collection<T>, Collection<T>) {
        let mut yes = Vec::new();
        let mut no = Vec::new();
        for item in self.items {
            if pred(&item) {
                yes.push(item);
            } else {
                no.push(item);
            }
        }
        (collect(yes), collect(no))
    }

    /// Group items by a key derived from each, preserving insertion order
    /// within each group.
    pub fn group_by<K: Eq + Hash, F: FnMut(&T) -> K>(
        self,
        mut key: F,
    ) -> HashMap<K, Collection<T>> {
        let mut groups: HashMap<K, Collection<T>> = HashMap::new();
        for item in self.items {
            let k = key(&item);
            groups.entry(k).or_default().items.push(item);
        }
        groups
    }

    // --- side effects / escape hatches --------------------------------------

    /// Run `f` on each item for its side effect, without consuming the
    /// collection — returns `self` for chaining.
    pub fn each<F: FnMut(&T)>(self, mut f: F) -> Collection<T> {
        for item in &self.items {
            f(item);
        }
        self
    }

    /// Hand the whole collection to `f` for a side effect, then return it —
    /// useful for logging or asserting mid-chain.
    pub fn tap<F: FnOnce(&Collection<T>)>(self, f: F) -> Collection<T> {
        f(&self);
        self
    }

    /// Pipe the whole collection into `f` and return whatever it produces —
    /// the fluent way out of a chain into an arbitrary value.
    pub fn pipe<R, F: FnOnce(Collection<T>) -> R>(self, f: F) -> R {
        f(self)
    }

    // --- folds --------------------------------------------------------------

    /// Fold the items into a single accumulator.
    pub fn reduce<A, F: FnMut(A, T) -> A>(self, init: A, f: F) -> A {
        self.items.into_iter().fold(init, f)
    }

    /// Sum a numeric projection of each item.
    pub fn sum_by<N, F: FnMut(&T) -> N>(&self, f: F) -> N
    where
        N: std::iter::Sum,
    {
        self.items.iter().map(f).sum()
    }
}

// --- bounded operations: available when T carries the needed trait ----------

impl<T: PartialEq> Collection<T> {
    /// `true` if any item equals `needle`.
    pub fn contains(&self, needle: &T) -> bool {
        self.items.contains(needle)
    }

    /// Drop later duplicates, keeping first occurrences in order. O(n²);
    /// prefer [`unique_hashed`](Self::unique_hashed) when `T: Eq + Hash`.
    pub fn unique(self) -> Collection<T> {
        let mut seen: Vec<T> = Vec::new();
        for item in self.items {
            if !seen.contains(&item) {
                seen.push(item);
            }
        }
        collect(seen)
    }
}

impl<T: Eq + Hash + Clone> Collection<T> {
    /// Drop duplicates in O(n) using a hash set, keeping first occurrences.
    pub fn unique_hashed(self) -> Collection<T> {
        let mut seen = std::collections::HashSet::new();
        collect(
            self.items
                .into_iter()
                .filter(move |x| seen.insert(x.clone())),
        )
    }
}

impl<T: Ord> Collection<T> {
    /// Sort in ascending order.
    pub fn sort(mut self) -> Collection<T> {
        self.items.sort();
        self
    }

    /// Largest item, if any.
    pub fn max(&self) -> Option<&T> {
        self.items.iter().max()
    }

    /// Smallest item, if any.
    pub fn min(&self) -> Option<&T> {
        self.items.iter().min()
    }
}

impl<T> Collection<T> {
    /// Sort by a derived, `Ord` key.
    pub fn sort_by_key<K: Ord, F: FnMut(&T) -> K>(mut self, f: F) -> Collection<T> {
        self.items.sort_by_key(f);
        self
    }

    /// Sort with an explicit comparator.
    pub fn sort_by<F: FnMut(&T, &T) -> std::cmp::Ordering>(mut self, cmp: F) -> Collection<T> {
        self.items.sort_by(cmp);
        self
    }
}

impl<T: std::iter::Sum> Collection<T> {
    /// Sum the items (numeric collections).
    pub fn sum(self) -> T {
        self.items.into_iter().sum()
    }
}

impl<T: std::fmt::Display> Collection<T> {
    /// Join the items into a single string separated by `glue`.
    pub fn implode(&self, glue: &str) -> String {
        self.items
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(glue)
    }
}

// --- interop: behave like the Vec it wraps -----------------------------------

impl<T> std::ops::Deref for Collection<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.items
    }
}

impl<T> From<Vec<T>> for Collection<T> {
    fn from(items: Vec<T>) -> Collection<T> {
        Collection { items }
    }
}

impl<T> From<Collection<T>> for Vec<T> {
    fn from(c: Collection<T>) -> Vec<T> {
        c.items
    }
}

impl<T> FromIterator<T> for Collection<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Collection<T> {
        collect(iter)
    }
}

impl<T> IntoIterator for Collection<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a Collection<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_filter_chain() {
        let out = collect(vec![1, 2, 3, 4, 5, 6])
            .filter(|n| n % 2 == 0)
            .map(|n| n * 10)
            .into_vec();
        assert_eq!(out, vec![20, 40, 60]);
    }

    #[test]
    fn reject_is_filter_inverse() {
        let (a, b) = (
            collect(vec![1, 2, 3, 4]).filter(|n| *n > 2).into_vec(),
            collect(vec![1, 2, 3, 4]).reject(|n| *n > 2).into_vec(),
        );
        assert_eq!(a, vec![3, 4]);
        assert_eq!(b, vec![1, 2]);
    }

    #[test]
    fn reduce_and_sum() {
        assert_eq!(collect(vec![1, 2, 3]).reduce(0, |a, n| a + n), 6);
        assert_eq!(collect(vec![1, 2, 3]).sum(), 6);
        assert_eq!(collect(vec!["a", "bb", "ccc"]).sum_by(|s| s.len()), 6);
    }

    #[test]
    fn implode_joins() {
        assert_eq!(collect(vec![1, 2, 3]).implode("-"), "1-2-3");
        assert_eq!(collect(Vec::<i32>::new()).implode(","), "");
    }

    #[test]
    fn unique_keeps_first_occurrence() {
        assert_eq!(
            collect(vec![1, 2, 2, 3, 1]).unique().into_vec(),
            vec![1, 2, 3]
        );
        assert_eq!(
            collect(vec![1, 2, 2, 3, 1]).unique_hashed().into_vec(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn chunk_splits_evenly_and_remainder() {
        let chunks: Vec<Vec<i32>> = collect(vec![1, 2, 3, 4, 5])
            .chunk(2)
            .map(|c| c.into_vec())
            .into_vec();
        assert_eq!(chunks, vec![vec![1, 2], vec![3, 4], vec![5]]);
    }

    #[test]
    fn partition_splits_by_predicate() {
        let (even, odd) = collect(vec![1, 2, 3, 4, 5]).partition(|n| n % 2 == 0);
        assert_eq!(even.into_vec(), vec![2, 4]);
        assert_eq!(odd.into_vec(), vec![1, 3, 5]);
    }

    #[test]
    fn group_by_key() {
        let groups =
            collect(vec!["ant", "bee", "arc", "bat"]).group_by(|s| s.chars().next().unwrap());
        let mut a = groups[&'a'].clone().into_vec();
        let mut b = groups[&'b'].clone().into_vec();
        a.sort();
        b.sort();
        assert_eq!(a, vec!["ant", "arc"]);
        assert_eq!(b, vec!["bat", "bee"]);
    }

    #[test]
    fn sort_variants() {
        assert_eq!(collect(vec![3, 1, 2]).sort().into_vec(), vec![1, 2, 3]);
        assert_eq!(
            collect(vec!["ccc", "a", "bb"])
                .sort_by_key(|s| s.len())
                .into_vec(),
            vec!["a", "bb", "ccc"]
        );
        assert_eq!(collect(vec![1, 3, 2]).max(), Some(&3));
        assert_eq!(collect(vec![1, 3, 2]).min(), Some(&1));
    }

    #[test]
    fn take_skip_reverse() {
        assert_eq!(collect(vec![1, 2, 3, 4]).take(2).into_vec(), vec![1, 2]);
        assert_eq!(collect(vec![1, 2, 3, 4]).skip(2).into_vec(), vec![3, 4]);
        assert_eq!(collect(vec![1, 2, 3]).reverse().into_vec(), vec![3, 2, 1]);
    }

    #[test]
    fn tap_and_pipe() {
        let mut peeked = 0;
        let n = collect(vec![1, 2, 3])
            .tap(|c| peeked = c.count())
            .pipe(|c| c.sum());
        assert_eq!(peeked, 3);
        assert_eq!(n, 6);
    }

    #[test]
    fn interop_roundtrips() {
        let c: Collection<i32> = vec![1, 2, 3].into();
        let v: Vec<i32> = c.clone().into();
        assert_eq!(v, vec![1, 2, 3]);
        // Deref to slice + IntoIterator over &Collection.
        assert_eq!(c.len(), 3);
        assert_eq!((&c).into_iter().copied().sum::<i32>(), 6);
        // FromIterator.
        let doubled: Collection<i32> = vec![1, 2, 3].into_iter().map(|n| n * 2).collect();
        assert_eq!(doubled.into_vec(), vec![2, 4, 6]);
    }
}
