//! Uses page rank to construct a local neighborhood, then fuses features.
use std::fmt::Write;

use rayon::prelude::*;
use hashbrown::HashMap;

use crate::algos::rwr::RWR;
use crate::algos::utils::{Sample,FeatureHasher};
use crate::embeddings::EmbeddingStore;
use crate::distance::Distance;
use crate::feature_store::FeatureStore;
use crate::graph::{CDFGraph,Graph};
use crate::progress::CLProgressBar;

pub struct PPREmbed {

    /// Number of random walks per node to estimate neighborhood
    pub num_walks: usize,

    /// Restart criteria
    pub steps: Sample,

    /// Beta Parameter 
    pub beta: f32,

    /// Dimensions for FeatureHashing
    pub dims: usize,

    /// Minimum weight to combine
    pub eps: f32,

    /// Random seed
    pub seed: u64,
}

impl PPREmbed {
    /// Learns the feature embeddings.
    pub fn learn<G: Graph + CDFGraph + Send + Sync>(
        &self, 
        graph: &G, 
        features: &FeatureStore
    ) -> EmbeddingStore {
        let embs = EmbeddingStore::new(graph.len(), self.dims, Distance::Cosine);
        let hasher = FeatureHasher::new(self.dims);
        let pb = CLProgressBar::new(graph.len() as u64, true);
        pb.update_message(|msg| write!(msg, "Embedding...").expect("Shouldn't fail"));
        (0..graph.len()).into_par_iter().for_each(|node_id| {
            let rwr = RWR {
                steps: self.steps,
                walks: self.num_walks,
                beta: self.beta,
                single_threaded: false,
                seed: self.seed + node_id as u64
            };

            let mut feat_maps = HashMap::new();
            rwr.sample_bfs(graph, node_id)
                .into_iter()
                .for_each(|(node_id, weight)| {
                    features.get_features(node_id).iter().for_each(|feat_id| {
                        let e = feat_maps.entry(*feat_id).or_insert(0f32);
                        *e += weight;
                    });
                });

            let emb = embs.get_embedding_mut_hogwild(node_id);
            let n = graph.len();
            feat_maps.into_iter()
                .filter(|(_,w)| *w > self.eps)
                .for_each(|(feat_id, weight)| {
                for hash_num in 0..3 {
                    let (sign, dim) = hasher.hash(feat_id, hash_num);
                    emb[dim] += sign as f32 * (weight * n as f32).ln().max(0f32);
                }
            });

            pb.inc(1);
        });
        pb.finish();

        embs
    }
}

