//! Round-robin reuse of a fixed set of items (the capture texture pool).

/// Hands out `N` items in order, least recently used first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RoundRobin<T, const N: usize> {
    items: [T; N],
}

impl<T, const N: usize> RoundRobin<T, N> {
    pub(crate) const fn new(items: [T; N]) -> Self {
        Self { items }
    }

    /// Returns the least recently returned item, or `None` when `N == 0`.
    pub(crate) const fn next(&mut self) -> Option<&T> {
        if N == 0 {
            return None;
        }
        self.items.rotate_left(1);
        self.items.last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_in_order() {
        let mut ring = RoundRobin::new([1, 2, 3]);
        let order: Vec<i32> = (0..7).map(|_| *ring.next().unwrap()).collect();
        assert_eq!(order, [1, 2, 3, 1, 2, 3, 1]);
    }

    #[test]
    fn item_is_reused_after_n_calls() {
        let mut ring = RoundRobin::new(['a', 'b', 'c']);
        let first = *ring.next().unwrap();
        ring.next();
        ring.next();
        assert_eq!(*ring.next().unwrap(), first);
    }

    #[test]
    fn single_and_empty() {
        let mut one = RoundRobin::new([7]);
        assert_eq!(one.next(), Some(&7));
        assert_eq!(one.next(), Some(&7));
        let mut none = RoundRobin::<u8, 0>::new([]);
        assert_eq!(none.next(), None);
    }
}
