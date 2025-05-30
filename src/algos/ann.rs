use std::cmp::{Ordering,Eq};
use std::collections::BinaryHeap;

use rand::prelude::*;
use rand_xorshift::XorShiftRng;
use rayon::prelude::*;
use float_ord::FloatOrd;

use crate::graph::NodeID;
use crate::embeddings::{EmbeddingStore,Entity};
use crate::algos::graph_ann::{NodeDistance,TopK};

#[inline(always)]
fn dot(x: &[f32], y: &[f32]) -> f32 {
    x.iter().zip(y.iter()).map(|(xi, yi)| xi * yi).sum()
}

struct Hyperplane {
    coef: Vec<f32>,
    bias: f32
}

impl Hyperplane {
    fn new(coef: Vec<f32>, bias: f32) -> Self {
        Hyperplane { coef, bias }
    }

    fn point_is_above(&self, emb: &[f32]) -> bool {
        self.distance(emb) >= 0.
    }

    fn distance(&self, emb: &[f32]) -> f32 {
        dot(&self.coef, emb) + self.bias
    }

}

type TreeIndex = usize;
type TreeTable = Vec<Tree>;

enum Tree {
    Leaf { indices: Vec<NodeID> },

    Split {
        hp: Hyperplane,
        above: TreeIndex,
        below: TreeIndex
    }
}

#[derive(Debug)]
struct HpDistance(f32, usize);

impl HpDistance {
    fn new(tree_idx: usize, score: f32) -> Self {
        HpDistance(score, tree_idx)
    }

}

impl Ord for HpDistance {
    fn cmp(&self, other: &Self) -> Ordering {
        FloatOrd(other.0).cmp(&FloatOrd(self.0)).then_with(|| other.1.cmp(&self.1))
    }
}

impl PartialOrd for HpDistance {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for HpDistance {
    fn eq(&self, other: &Self) -> bool {
        let s_cmp = (self.0, self.1);
        let o_cmp = (other.0, other.1);
        s_cmp == o_cmp
    }
}

impl Eq for HpDistance {}

fn tree_predict(
    tree_table: &TreeTable,
    es: &EmbeddingStore, 
    emb: &[f32],
    k: usize,
    mut min_search_nodes: usize
) -> Vec<(NodeID, f32)> {

    // Must explore at least K
    min_search_nodes = min_search_nodes.max(k);

    // The set of nodes to return
    let mut return_set = TopK::new(k);
    
    // heap to store tree splits
    let mut heap = BinaryHeap::with_capacity(k * 2);

    // Root node is last in the table.  Start exploring the root tree
    let tree_idx = tree_table.len() - 1;
    heap.push( HpDistance::new(tree_idx, 0.) );
    let mut buff = vec![0f32; k];

    let mut visited = 0usize;
    while let Some(HpDistance(_, tree_idx)) = heap.pop() {
        match &tree_table[tree_idx] {
            Tree::Leaf { ref indices } => {
                let n_nodes = indices.len();
                // Ensure temp buff is sufficiently sized
                while buff.len() < n_nodes {
                    buff.push(0f32);
                }

                // Score the nodes
                let qemb = Entity::Embedding(emb);
                
                indices.par_iter().zip(buff.par_iter_mut()).for_each(|(&node_id, b)| {
                    *b = es.compute_distance(&Entity::Node(node_id), &qemb);
                });

                indices.iter().zip(buff.iter()).for_each(|(node_id, dist)| {
                    return_set.push(*node_id, *dist);
                });

                visited += n_nodes;
            },
            Tree::Split { ref hp, ref above, ref below } => {
                let dist = hp.distance(emb);
                let above_dist = if dist >= 0.0 { 0.0 } else { dist.abs() };
                let below_dist = if dist < 0.0 { 0.0 } else { dist.abs() };
                heap.push(HpDistance::new(*above, above_dist));
                heap.push(HpDistance::new(*below, below_dist));
            }
        }
        if visited >= min_search_nodes { break }
    }

    return_set.into_sorted().into_iter().map(|nd| {
        nd.to_tup_cloned()
    }).collect()
}


fn tree_leaf_index(
    tree_table: &TreeTable,
    emb: &[f32]
) -> usize {
    let mut node = tree_table.len() - 1;
    loop {
        match &tree_table[node] {
            Tree::Leaf { indices: _ } => { return node },
            Tree::Split { ref hp, ref above, ref below } => {
                node = if hp.point_is_above(emb) { *above } else { *below };
            }
        }
    }
}

/**
 * Produces the path an embedding took through the ANN and returns it as
 * a path
 */
fn tree_leaf_path(
    tree_table: &TreeTable,
    emb: &[f32]
) -> Vec<usize> {
    let mut path = Vec::new();
    let mut node = tree_table.len() - 1;
    loop {
        match &tree_table[node] {
            Tree::Leaf { indices: _ } => { break },
            Tree::Split { ref hp, ref above, ref below } => {
                node = if hp.point_is_above(emb) { *above } else { *below };
            }
        }
        path.push(node);
    }
    path
}

/**
 * Computes the max depth of a tree
 */
fn tree_depth(
    tree_table: &TreeTable,
    node: TreeIndex
) -> usize {
    match &tree_table[node] {
        Tree::Leaf { indices: _ } =>  1,
        Tree::Split { hp: _, above, below } => {
            let above_depth = tree_depth(tree_table, *above);
            let below_depth = tree_depth(tree_table, *below);
            above_depth.max(below_depth) + 1
        }
    }
}

pub struct AnnBuildConfig {
    max_nodes_per_leaf: usize,
    test_hp_per_split: usize,
    num_sampled_nodes_split_test: usize
}

/** Implements an ANN based on random hyperplanes.  It offers the advantage of also
 * producing leaf index transforms, which can be suitable for indexing in traditional 
 * inverted indexs
 */
pub struct Ann {
    trees: Vec<TreeTable>
}

impl Ann {
    pub fn new() -> Self {
        Ann { trees: Vec::new() }
    }

    pub fn fit(
        &mut self,
        es: &EmbeddingStore,
        n_trees: usize,
        max_nodes_per_leaf: usize,
        test_hp_per_split: Option<usize>,
        num_sampled_nodes_split_test: Option<usize>,
        node_ids: Option<Vec<NodeID>>,
        seed: u64
    ) {
        let config = AnnBuildConfig {
            max_nodes_per_leaf: max_nodes_per_leaf,
            test_hp_per_split: test_hp_per_split.unwrap_or(5),
            num_sampled_nodes_split_test: num_sampled_nodes_split_test.unwrap_or(30)
        };

        // Setup the number of trees necessary to build
        let mut trees = Vec::with_capacity(n_trees);
        for _ in 0..n_trees {
            trees.push(Vec::new());
        }

        // Learn each tree, using separate random seeds
        trees.par_iter_mut().enumerate().for_each(|(idx, tree) | {
            let mut indices: Vec<_> = if let Some(nids) = node_ids.as_ref() {
                nids.iter().map(|idx| (*idx, false)).collect()
            } else {
                (0..es.len()).map(|idx| (idx, false)).collect()
            };
            let mut rng = XorShiftRng::seed_from_u64(seed + idx as u64);
            self.fit_group_(&config, tree, 1, es, indices.as_mut_slice(), &mut rng);
        });

        self.trees = trees;

    }

    pub fn depth(&self) -> Vec<usize> {
        self.trees.par_iter().map(|t| tree_depth(t, t.len() - 1)).collect()
    }

    fn fit_group_(
        &self, 
        config: &AnnBuildConfig,
        tree_table: &mut TreeTable,
        depth: usize,
        es: &EmbeddingStore,
        indices: &mut [(NodeID, bool)],
        rng: &mut impl Rng
    ) -> TreeIndex {
        if indices.len() < config.max_nodes_per_leaf {
            let node_ids = indices.iter().map(|(node_id, _)| *node_id).collect();
            tree_table.push(Tree::Leaf { indices: node_ids });
            return tree_table.len() - 1
        }

        let hp = if config.test_hp_per_split > 0 {
            compute_simple_splits(
                &(*indices), 
                es, 
                config.test_hp_per_split, 
                config.num_sampled_nodes_split_test,
                rng
            )
        } else {
            compute_normal_rp(
                &(*indices),
                es,
                config.num_sampled_nodes_split_test,
                rng
            )
        };
        // Score the nodes and get the number below the hyperplane
        let split_idx: usize = indices.par_iter_mut().map(|v| {
            v.1 = hp.point_is_above(es.get_embedding(v.0));
            if v.1 { 0 } else { 1 }
        }).sum();

        // Fast sort - we do this to save memory allocations
        sort_binary(indices);

        let (below, above) = indices.split_at_mut(split_idx);

        if above.len() > 0 && below.len() > 0 {
            let above_idx = self.fit_group_(config, tree_table, depth + 1, es, above, rng);
            let below_idx = self.fit_group_(config, tree_table, depth + 1, es, below, rng);

            tree_table.push(Tree::Split { hp: hp, above: above_idx, below: below_idx })

        } else {
            let node_ids = indices.iter().map(|(node_id, _)| *node_id).collect();
            tree_table.push(Tree::Leaf { indices: node_ids })
        }

        tree_table.len() - 1
    }

    pub fn predict(
        &self, 
        es: &EmbeddingStore, 
        emb: &[f32],
        k: usize,
        min_search_nodes: Option<usize>
    ) -> Vec<NodeDistance> {
        
        // Get the scores
        let min_search = min_search_nodes.unwrap_or(self.trees.len() * k);
        let scores = self.trees.par_iter().map(|tree| {
            tree_predict(tree, es, emb, k, min_search)
        }).collect::<Vec<_>>();

        // Fold them into a single vec
        let n = scores.iter().map(|x| x.len()).sum::<usize>();
        let mut all_scores = Vec::with_capacity(n);
        scores.into_iter().for_each(|subset| {
            subset.into_iter().for_each(|(node_id, s)| {
                all_scores.push(NodeDistance::new(s, node_id));
            });
        });

        // Sort by Score and Node ID
        all_scores.par_sort();

        // Deduplicate nodes which show up in multiple trees
        let mut cur_pointer = 1;
        let mut cur_node_id = all_scores[0].1;
        for i in 1..n {
            let next_id = all_scores[i].1;
            if next_id != cur_node_id {
                all_scores[cur_pointer] = all_scores[i];
                cur_node_id = next_id;
                cur_pointer += 1;
            }
        }

        all_scores.truncate(cur_pointer);
        all_scores.reverse();
        all_scores.truncate(k);
        all_scores
    }

    pub fn predict_leaf_indices(
        &self,
        emb: &[f32]
    ) -> Vec<usize> {
        self.trees.par_iter().map(|tree| {
            tree_leaf_index(tree, emb)
        }).collect()
    }

    pub fn predict_leaf_paths(
        &self,
        emb: &[f32]
    ) -> Vec<Vec<usize>> {
        self.trees.par_iter().map(|tree| {
            tree_leaf_path(tree, emb)
        }).collect()
    }

    pub fn num_trees(&self) -> usize {
        self.trees.len()
    }

}

fn sort_binary(vec: &mut [(NodeID, bool)]) {
    let mut low = 0;
    for cur_ptr in 0..vec.len() {
        if !vec[cur_ptr].1 {
            (vec[cur_ptr], vec[low]) = (vec[low], vec[cur_ptr]);
            //let cur_low = vec[low];
            //vec[low] = vec[cur_ptr];
            //vec[cur_ptr] = cur_low;
            low += 1;
        }
    }
}

fn compute_w_vec_from_points<'a>(
    indices: &[(NodeID, bool)], 
    es: &'a EmbeddingStore,
    rng: &mut impl Rng
) -> (Vec<f32>, &'a [f32], &'a [f32]) {
 
    // Select two points to create the hyperplane
    let idx_1 = indices.choose(rng).unwrap().0;
    let mut idx_2 = idx_1;
    while idx_1 == idx_2 {
        idx_2 = indices.choose(rng).unwrap().0;
    }

    let pa = es.get_embedding(idx_1); 
    let pb = es.get_embedding(idx_2); 

    // Compute the hyperplane
    let delta = pa.iter().zip(pb.iter()).map(|(pai, pbi)| pai - pbi).collect();
    (delta, pa, pb)
}

fn update_point(centroid: &mut [f32], new_point: &[f32], count: usize) {
    centroid.iter_mut().zip(new_point.iter()).for_each(|(ci, npi)| {
        let r = (count - 1) as f32 / count as f32;
        *ci = r * *ci + (1f32 - r) * npi;
    });
}

fn pseudo_kmeans_w_vec_from_points<'a>(
    indices: &[(NodeID, bool)], 
    es: &EmbeddingStore,
    iterations: usize,
    rng: &mut impl Rng
) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
 
    // Select two initial points for seeding the centroid
    let idx_1 = indices.choose(rng).unwrap().0;
    let mut idx_2 = idx_1;
    while idx_1 == idx_2 {
        idx_2 = indices.choose(rng).unwrap().0;
    }

    let mut pa = es.get_embedding(idx_1).to_vec(); 
    let mut pb = es.get_embedding(idx_2).to_vec(); 

    let d = es.distance();
    let (mut ac, mut bc) = (1usize, 1usize);
    for _ in 0..iterations {
        let idx = indices.choose(rng).unwrap().0;
        let emb = es.get_embedding(idx);
        let da = ac as f32 * d.compute(&pa, emb);
        let db = bc as f32 * d.compute(&pb, emb);
        if da > db {
            bc += 1;
            update_point(&mut pb, emb, bc);
        } else {
            ac += 1;
            update_point(&mut pa, emb, ac);
        }
    }

    // Compute the hyperplane
    let delta = pa.iter().zip(pb.iter()).map(|(pai, pbi)| pai - pbi).collect();
    (delta, pa, pb)
}

fn compute_simple_splits(
    indices: &[(NodeID, bool)], 
    es: &EmbeddingStore,
    test_hp_per_split: usize,
    num_sampled_nodes_split_test: usize,
    rng: &mut impl Rng
) -> Hyperplane {
    let n = num_sampled_nodes_split_test.min(indices.len());

    let mut best = (0usize, None);
    // Try several different candidates and select the hyperplane that divides them the best
    for _ in 0..test_hp_per_split {

        //let (diff, pa, pb) = compute_w_vec_from_points(indices, es, rng);
        let (diff, pa, pb) = pseudo_kmeans_w_vec_from_points(indices, es, num_sampled_nodes_split_test, rng);
        
        // Figure out the vector bias
        let bias: f32 = diff.iter().zip(pa.iter().zip(pb.iter()))
            .map(|(d, (pai, pbi))| d * (pai + pbi) / 2.)
            .sum();

        // Count the number of instances on each side of the hyperplane from random points
        let hp = Hyperplane::new(diff, bias);
        let mut s = 0usize;
        for _ in 0..n {
            let idx = indices.choose(rng).unwrap().0;
            let emb = es.get_embedding(idx);
            if hp.point_is_above(emb) { s += 1; } 
        }
         
        let delta = n - s;
        let score = s.max(delta) - s.min(delta);
        if score < best.0 || best.1.is_none() {
            best = (score, Some(hp));
        }
    }
    best.1.unwrap()
}

fn median(deltas: &[f32]) -> f32 {
    assert!(deltas.len() > 0);
    let is_odd = deltas.len() % 2 == 1;
    let half = deltas.len() / 2;
    if is_odd {
        deltas[half]
    } else {
        (deltas[half - 1] + deltas[half]) / 2f32
    }
}

fn compute_normal_rp(
    indices: &[(NodeID, bool)], 
    es: &EmbeddingStore,
    num_sampled_nodes_split_test: usize,
    rng: &mut impl Rng
) -> Hyperplane {
    let random_vec = compute_w_vec_from_points(indices, es, rng).0;
    //let random_vec = create_normalized_vec(es.dims(), rng);
    let n = num_sampled_nodes_split_test.min(indices.len());
    let mut rps = vec![0f32; n];

    rps.iter_mut().for_each(|rp_i| {
        let idx = indices.choose(rng).unwrap().0;
        let emb_i = es.get_embedding(idx);
        *rp_i = dot(emb_i, random_vec.as_slice());
    });
    rps.sort_by_key(|v| FloatOrd(*v));
    let bias = -median(rps.as_slice());
    Hyperplane::new(random_vec, bias)
}
