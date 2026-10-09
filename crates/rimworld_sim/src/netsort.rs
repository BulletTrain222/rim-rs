//! The .NET runtime's unstable sort (`List<T>.Sort` / `Array.Sort`, an
//! introspective sort), reproduced so that orders the game builds with it
//! — where equal elements end up in an arbitrary but fixed order — come
//! out the same here (docs/research.md §52).
//!
//! The algorithm: quicksort with a median-of-three pivot moved next to the
//! end; ranges of at most 16 go to insertion sort (2 and 3 elements by
//! compare-and-swap); after 2 × (⌊log₂ n⌋ + 1) levels of partitioning a
//! range is heapsorted instead.

use std::cmp::Ordering;

/// Sorts `v` exactly as .NET's `IntrospectiveSort` would with `cmp`.
pub fn sort<T>(v: &mut [T], cmp: impl Fn(&T, &T) -> Ordering) {
    if v.len() < 2 {
        return;
    }
    let mut bits = 0;
    let mut n = v.len();
    while n >= 1 {
        bits += 1;
        n /= 2;
    }
    intro(v, 0, v.len() - 1, 2 * bits, &cmp);
}

fn swap_if_greater<T>(v: &mut [T], cmp: &impl Fn(&T, &T) -> Ordering, a: usize, b: usize) {
    if a != b && cmp(&v[a], &v[b]) == Ordering::Greater {
        v.swap(a, b);
    }
}

fn intro<T>(
    v: &mut [T],
    lo: usize,
    mut hi: usize,
    mut depth: u32,
    cmp: &impl Fn(&T, &T) -> Ordering,
) {
    while hi > lo {
        let size = hi - lo + 1;
        if size <= 16 {
            match size {
                2 => swap_if_greater(v, cmp, lo, hi),
                3 => {
                    swap_if_greater(v, cmp, lo, hi - 1);
                    swap_if_greater(v, cmp, lo, hi);
                    swap_if_greater(v, cmp, hi - 1, hi);
                }
                _ => insertion(v, lo, hi, cmp),
            }
            return;
        }
        if depth == 0 {
            heapsort(v, lo, hi, cmp);
            return;
        }
        depth -= 1;
        let p = partition(v, lo, hi, cmp);
        intro(v, p + 1, hi, depth, cmp);
        if p == 0 {
            return;
        }
        hi = p - 1;
    }
}

/// Median of the first, middle and last element as the pivot, parked at
/// `hi - 1`; scan inwards from both ends swapping out-of-place pairs;
/// the pivot lands where the scans meet.
fn partition<T>(v: &mut [T], lo: usize, hi: usize, cmp: &impl Fn(&T, &T) -> Ordering) -> usize {
    let mid = lo + (hi - lo) / 2;
    swap_if_greater(v, cmp, lo, mid);
    swap_if_greater(v, cmp, lo, hi);
    swap_if_greater(v, cmp, mid, hi);
    v.swap(mid, hi - 1);
    let pivot = hi - 1;
    let (mut left, mut right) = (lo, hi - 1);
    while left < right {
        left += 1;
        while cmp(&v[left], &v[pivot]) == Ordering::Less {
            left += 1;
        }
        right -= 1;
        while cmp(&v[pivot], &v[right]) == Ordering::Less {
            right -= 1;
        }
        if left >= right {
            break;
        }
        v.swap(left, right);
    }
    if left != hi - 1 {
        v.swap(left, hi - 1);
    }
    left
}

/// Straight insertion: each element shifts left past the greater ones.
fn insertion<T>(v: &mut [T], lo: usize, hi: usize, cmp: &impl Fn(&T, &T) -> Ordering) {
    for i in lo..hi {
        // v[i + 1] moves left while it is less than its left neighbour.
        let mut j = i + 1;
        while j > lo && cmp(&v[j], &v[j - 1]) == Ordering::Less {
            v.swap(j - 1, j);
            j -= 1;
        }
    }
}

/// Max-heap over the range (1-based node numbering), then repeated
/// extraction to the end.
fn heapsort<T>(v: &mut [T], lo: usize, hi: usize, cmp: &impl Fn(&T, &T) -> Ordering) {
    let n = hi - lo + 1;
    for i in (1..=n / 2).rev() {
        down_heap(v, i, n, lo, cmp);
    }
    for end in (2..=n).rev() {
        v.swap(lo, lo + end - 1);
        down_heap(v, 1, end - 1, lo, cmp);
    }
}

fn down_heap<T>(v: &mut [T], mut i: usize, n: usize, lo: usize, cmp: &impl Fn(&T, &T) -> Ordering) {
    // The sifted element moves down by swaps (the .NET code shifts a hole,
    // which ends in the same arrangement).
    while i <= n / 2 {
        let mut child = 2 * i;
        if child < n && cmp(&v[lo + child - 1], &v[lo + child]) == Ordering::Less {
            child += 1;
        }
        if cmp(&v[lo + i - 1], &v[lo + child - 1]) != Ordering::Less {
            break;
        }
        v.swap(lo + i - 1, lo + child - 1);
        i = child;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_like_any_sort_for_distinct_keys() {
        let mut v: Vec<i32> = (0..1000).map(|i| (i * 7919) % 1009).collect();
        sort(&mut v, |a, b| a.cmp(b));
        let mut w = v.clone();
        w.sort();
        assert_eq!(v, w);
    }

    #[test]
    fn orders_equal_keys_like_the_game() {
        // The game's radial pattern: offsets from (-80..80)² in x-major
        // order, sorted by squared length. The `place` probe showed (0,1)
        // before (0,-1) before (-1,0) among the distance-1 cells.
        let pattern = crate::clean::radial_pattern();
        assert_eq!(pattern[0], crate::grid::Cell::new(0, 0));
        let pos = |x, z| {
            pattern
                .iter()
                .position(|&c| c == crate::grid::Cell::new(x, z))
                .unwrap()
        };
        assert!(pos(0, 1) < pos(0, -1));
        assert!(pos(0, -1) < pos(-1, 0));
    }
}
