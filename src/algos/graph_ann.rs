//! This module defines a mediocre way of approximating nearest neighbors through random graph
//! traversals and greedy hill climbing toward embeddings which minimize the distance.  It's fine,
//! works better than nothing, but leaves a lot to be desired and is entirely dependent on the
//! connectedness of the graph.  Meh.
use std::cmp::{Eq,PartialEq,Ordering,Reverse};
use std::collections::BinaryHeap;

use hashbrown::HashSet;
use rand::prelude::*;
use rand_xorshift::XorShiftRng;
use rand_distr::{Distribution,Uniform};
use float_ord::FloatOrd;

use crate::graph::{Graph as CGraph,NodeID};
use crate::embeddings::{EmbeddingStore,Entity};

/// Defines a distance metric which we can use with heaps.  Lower == better

#[derive(Copy, Clone, Debug)]
pub struct DistanceFromEntity<A>(pub f32, pub A);

impl <A> DistanceFromEntity<A> {
    pub fn to_tup(&self) -> (&A, f32) {
        (&self.1, self.0)
    }

    pub fn new(distance: f32, entity: A) -> DistanceFromEntity<A> {
        DistanceFromEntity(distance, entity)
    }
}

impl <A: Clone> DistanceFromEntity<A> {
    pub fn to_tup_cloned(&self) -> (A, f32) {
        (self.1.clone(), self.0)
    }
}

// Min Heap, so reverse order
impl <A: Ord> Ord for DistanceFromEntity<A> {
    fn cmp(&self, other: &Self) -> Ordering {
        FloatOrd(other.0).cmp(&FloatOrd(self.0))
            .then_with(|| other.1.cmp(&self.1))
    }
}

impl <A: Ord> PartialOrd for DistanceFromEntity<A> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl <A: PartialEq> PartialEq for DistanceFromEntity<A> {
    fn eq(&self, other: &Self) -> bool {
        FloatOrd(self.0) == FloatOrd(other.0) && self.1 == other.1
    }
}

impl <A: Eq> Eq for DistanceFromEntity<A> {}

pub type NodeDistance = DistanceFromEntity<NodeID>;

/// Struct which tracks the top K nodes according to some distance.  Useful outside of ANN as well.
pub struct TopK {
    heap: BinaryHeap<Reverse<NodeDistance>>,
    k: usize
}

impl TopK {
    pub fn new(k: usize) -> Self {
        TopK {
            k: k,
            heap: BinaryHeap::with_capacity(k+1)
        }
    }

    pub fn push(&mut self, node_id: NodeID, score: f32) {
        self.push_nd(Reverse(NodeDistance::new(score, node_id)));
    }

    fn push_nd(&mut self, nd: Reverse<NodeDistance>) {
        self.heap.push(nd);
        if self.heap.len() > self.k {
            self.heap.pop();
        }
    }

    pub fn into_sorted(self) -> Vec<NodeDistance> {
        let mut results: Vec<NodeDistance> = self.heap.into_iter()
            .map(|n| n.0).collect();
        results.sort_by_key(|n| FloatOrd(n.0));
        results
    }

    pub fn extend(&mut self, other: TopK) {
        other.heap.into_iter().for_each(|nd| {
            self.push_nd(nd);
        });
    }

    pub fn len(&self) -> usize {
        self.heap.len()
    }
}

/// This Ann hill climbs from random starting nodes within the graph.  if the graph isn't fully
/// connected, good luck.  Depending on the smoothness of the embeddings amongst neighbors, has a
/// habit of running into local minimas.  It's fine, just not anything special.
#[derive(Debug)]
pub struct Ann {
    k: usize,
    max_steps: usize,
    seed: u64
}

impl Ann {
    pub fn new(k: usize, max_steps: usize, seed: u64) -> Self {
        Ann {k, max_steps, seed}
    }

    pub fn find<G: CGraph + Send + Sync>(
        &self, 
        query: &[f32],
        graph: &G, 
        embeddings: &EmbeddingStore,
    ) -> Vec<NodeDistance> {
        let mut rng = XorShiftRng::seed_from_u64(self.seed);
        hill_climb(
            Entity::Embedding(query), 
            graph,
            embeddings,
            self.k,
            self.max_steps,
            &mut rng)
    }
    
}

// This hill climbs.  We start with a node and compute the embeddings for each node.  We greedily
// explore the edges where the distance is minmized.  We return the best nodes after performing the
// search `max_steps` times.
fn hill_climb<'a, G: CGraph, R: Rng>(
    needle: Entity<'a>, 
    graph: &G, 
    es: &EmbeddingStore,
    k: usize,
    mut max_steps: usize,
    rng: &mut R
) -> Vec<NodeDistance> {
    let distribution = Uniform::new(0, graph.len());

    let mut heap = BinaryHeap::new();
    let mut best = TopK::new(k);
    let mut seen = HashSet::new();

    while max_steps > 0 {

        // Find a starting node, randomly selected
        heap.clear();
        let start_node = distribution.sample(rng);
        seen.insert(start_node.clone());
        let start_d = es.compute_distance(&needle, &Entity::Node(start_node.clone()));
        let start = NodeDistance::new(start_d, start_node);
        heap.push(start.clone());

        loop {
            // Hardcoded restart rate for the time being
            if rng.gen::<f32>() < 0.05f32 {
                break
            }

            let cur_node = heap.pop().expect("Shouldn't be empty!");
            best.push(cur_node.1, cur_node.0);
            // Get edges, compute distances between them and needle, add to the heap
            for edge in graph.get_edges(cur_node.1).0.iter() {
                if !seen.contains(edge) {
                    seen.insert(*edge);
                    let dist = es.compute_distance(&needle, &Entity::Node(*edge));
                    heap.push(NodeDistance::new(dist, *edge));
                }
            }

            max_steps -= 1;
            if max_steps == 0 || heap.len() == 0 {
                break
            }
        }

    }
    best.into_sorted()
}

#[cfg(test)]
mod ann_tests {
    use super::*;

    fn build_star_edges() -> Vec<(usize, usize, f32)> {
        let mut edges = Vec::new();
        let max = 100;
        for ni in 0..max {
            for no in (ni+1)..max {
                edges.push((ni, no, 1f32));
                edges.push((no, ni, 1f32));
            }
        }
        edges
    }

    #[test]
    fn test_top_k() {
        let mut top_k = TopK::new(3);
        top_k.push(1, 0.1);
        top_k.push(2, 0.2);
        top_k.push(3, 0.3);
        top_k.push(4, 0.15);
        top_k.push(5, 0.01);

        let results = top_k.into_sorted();

        println!("results: {:?}", results);
        assert_eq!(results[0], NodeDistance(0.01, 5));
        assert_eq!(results[1], NodeDistance(0.1, 1));
        assert_eq!(results[2], NodeDistance(0.15, 4));
    }
}
