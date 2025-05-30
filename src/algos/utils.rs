/// Random utility functions
use std::hash::{Hash,Hasher};
use std::cmp::{Ordering,PartialOrd,Eq,PartialEq,Reverse};
use std::collections::BinaryHeap;

use float_ord::FloatOrd;
use rand::prelude::*;
use rand_distr::{Uniform,Binomial};
use ahash::AHasher;

use crate::NodeID;

/// Counts a set of items by id.  See the test for examples.
pub struct Counter<'a> {
    slice: &'a [usize],
    idx: usize
}

impl <'a> Counter<'a> {
    pub fn new(slice: &'a [usize]) -> Self {
        Counter {
            slice,
            idx: 0
        }
    }
}

impl <'a> Iterator for Counter<'a> {
    type Item = (usize, usize);
    fn next(&mut self) -> Option<Self::Item> {
        let start = self.idx;
        let mut count = 0;
        for _ in self.idx..self.slice.len() {
            if self.slice[self.idx] != self.slice[start] { 
                return Some((self.slice[start], count)) 
            }
            self.idx += 1;
            count += 1;
        }
        if count > 0 {
            return Some((self.slice[start], count)) 
        } 
        None
    }
}

pub fn get_best_count<R: Rng>(counts: &[usize], rng: &mut R) -> usize {
    let mut best_count = 0;
    let mut ties = Vec::new();
    for (cluster, count) in Counter::new(counts) {
        if count > best_count {
            best_count = count;
            ties.clear();
            ties.push(cluster)
        } else if count == best_count {
            ties.push(cluster)
        }
    }

    // We tie break by randomly choosing an item
    if ties.len() > 1 {
        *ties.as_slice()
            .choose(rng)
            .expect("If a node has no edges, code bug")
    } else {
        ties[0]
    }

}

pub struct FeatureHasher {
    dims: usize
}

impl FeatureHasher {

    pub fn new(dims: usize) -> Self {
        FeatureHasher { dims }
    }

    #[inline]
    pub fn hash(
        &self,
        feature: usize, 
        hash_num: usize
    ) -> (i8, usize) {
        self.compute_sign_idx(feature, hash_num)
    }

    #[inline(always)]
    fn calculate_hash<T: Hash>(t: T) -> u64 {
        let mut s = AHasher::default();
        t.hash(&mut s);
        s.finish()
    }

    #[inline]
    fn compute_sign_idx(&self, feat: usize, hash_num: usize) -> (i8, usize) {
        let hash = FeatureHasher::calculate_hash((feat, hash_num)) as usize;
        let sign = (hash & 1) as i8;
        let idx = (hash >> 1) % self.dims as usize;
        (2 * sign - 1, idx)
    }
}

struct OrdFirst<A,B>(A, B);

impl <A:PartialEq,B> PartialEq for OrdFirst<A,B> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl <A:Eq,B> Eq for OrdFirst<A,B> {}

impl <A:PartialOrd,B> PartialOrd for OrdFirst<A,B> {
    fn partial_cmp(&self, other: &OrdFirst<A,B>) -> Option<Ordering> {
        self.0.partial_cmp(&other.0)
    }
}

impl <A:Ord,B> Ord for OrdFirst<A,B> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

pub fn reservoir_sample(
    it: impl Iterator<Item=(NodeID, f32)>,
    size: usize,
    rng: &mut impl Rng
) -> Vec<(NodeID, f32)> {
    let mut sample = Vec::with_capacity(size);
    for (i, n) in it.enumerate() {
        if i < size {
            sample.push(n);
        } else {
            let idx = Uniform::new(0, i).sample(rng);
            if idx < size {
                sample[idx] = n;
            }
        }
    }
    sample
}

pub fn weighted_reservoir_sample<A>(
    items: impl Iterator<Item=(A, f32)>,
    n: usize,
    rng: &mut impl Rng
) -> Vec<(A, f32)> {
    let mut bh = BinaryHeap::with_capacity(n+1);
    for (i, (item, weight)) in items.enumerate() {
        let nw = rng.gen::<f32>().powf(1f32 / weight);
        bh.push(Reverse(OrdFirst(FloatOrd(nw), (item, weight))));
        if i >= n {
            bh.pop();
        }
    }
    bh.into_iter().map(|of| of.0.1).collect()
}

pub struct IllegalSample;

#[derive(Clone,Copy,Debug)]
pub enum Sample {
    All,
    Fixed(usize),
    Probability(f32)
}

impl Sample {
    pub fn new(n: f32) -> Result<Sample,IllegalSample> {
        if n < 0f32 {
            Err(IllegalSample)
        } else {
            let s = if n > 0f32 && n < 1f32 {
                Sample::Probability(n)
            } else { 
                Sample::Fixed(n as usize)
            };
            Ok(s)
        }
    }

    pub fn all() -> Sample {
        Sample::All
    }

    /// Samples from a Sample, returning both the number of entries
    /// to sample as well as the scalar for drop out.  Probability is
    /// interpretted as the expected number of success.
    pub fn sample(
        &self,
        n: usize,
        at_least_one: bool,
        rng: &mut impl Rng
    ) -> (usize, f32) {
        match self {
            Sample::Fixed(k) => { 
                let r = (*k).min(n);
                (r, n as f32 / r as f32) 
            },
            Sample::Probability(p) => { 
                let dist = Binomial::new(n as u64, *p as f64).unwrap();
                let k = dist.sample(rng);
                if k == 0 && at_least_one {
                    (1, (1f32 - p.powf(n as f32)) / p)
                } else {
                    (k as usize, 1f32 / p)
                }
            },
            Sample::All => { (n, 1f32) }

        }
    }
}


#[cfg(test)]
mod utils_tests {
    use super::*;
    use rand_xorshift::XorShiftRng;

    #[test]
    fn test_choose_best() {
        let counts = vec![0, 0, 1, 1, 1, 2];
        let mut rng = XorShiftRng::seed_from_u64(1);
        let best_count = get_best_count(&counts, &mut rng);
        assert_eq!(best_count, 1);
    }

    #[test]
    fn test_choose_one() {
        let counts = vec![0, 0, 0];
        let mut rng = XorShiftRng::seed_from_u64(1);
        let best_count = get_best_count(&counts, &mut rng);
        assert_eq!(best_count, 0);
    }

    #[test]
    fn test_choose_between() {
        let counts = vec![0, 1];
        let mut rng = XorShiftRng::seed_from_u64(1231232132);
        let best_count = get_best_count(&counts, &mut rng);
        assert_eq!(best_count, 0);
        let best_count = get_best_count(&counts, &mut rng);
        assert_eq!(best_count, 1);
        let best_count = get_best_count(&counts, &mut rng);
        assert_eq!(best_count, 1);
        let best_count = get_best_count(&counts, &mut rng);
        assert_eq!(best_count, 0);
    }

    #[test]
    fn test_choose_last() {
        let counts = vec![0, 1, 1];
        let mut rng = XorShiftRng::seed_from_u64(1231232132);
        let best_count = get_best_count(&counts, &mut rng);
        assert_eq!(best_count, 1);
    }

    #[test]
    fn test_choose_only() {
        let counts = vec![0, 0, 0];
        let mut rng = XorShiftRng::seed_from_u64(1231232132);
        let best_count = get_best_count(&counts, &mut rng);
        assert_eq!(best_count, 0);
    }

    #[test]
    fn test_counter() {
        let counts = [0, 0, 0, 1, 2, 2, 3];
        let mut counter = Counter::new(&counts);
        assert_eq!(counter.next(), Some((0, 3)));
        assert_eq!(counter.next(), Some((1, 1)));
        assert_eq!(counter.next(), Some((2, 2)));
        assert_eq!(counter.next(), Some((3, 1)));
        assert_eq!(counter.next(), None);

        let counts = [];
        let mut counter = Counter::new(&counts);
        assert_eq!(counter.next(), None);

        let counts = [0];
        let mut counter = Counter::new(&counts);
        assert_eq!(counter.next(), Some((0, 1)));
        assert_eq!(counter.next(), None);

    }

}

