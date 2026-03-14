use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use arc_swap::ArcSwap;

use crate::graph::persist_stats::ProjectGraphPersistStats;
use crate::graph::snapshot::ProjectGraphSnapshot;
use crate::graph::state::ProjectGraphState;
use crate::graph::store::{ProjectGraphStore, ProjectGraphStoreOptions};

pub struct ProjectGraphRuntime {
    store: ProjectGraphStore,
    snapshot: ArcSwap<ProjectGraphSnapshot>,
}

impl ProjectGraphRuntime {
    pub fn open_with_options(
        path: impl AsRef<Path>,
        options: ProjectGraphStoreOptions,
    ) -> Result<Self> {
        let store = ProjectGraphStore::open_with_options(path, options)?;
        let snapshot = Arc::new(store.load_snapshot()?);
        Ok(Self {
            store,
            snapshot: ArcSwap::from(snapshot),
        })
    }

    pub fn snapshot(&self) -> Arc<ProjectGraphSnapshot> {
        self.snapshot.load_full()
    }

    pub fn update_state_with_stats<F>(
        &self,
        mutator: F,
    ) -> Result<(Arc<ProjectGraphSnapshot>, ProjectGraphPersistStats)>
    where
        F: FnOnce(&mut ProjectGraphState),
    {
        let mut state = self.snapshot().to_state_clone();
        mutator(&mut state);
        let (next_snapshot, stats) = self.store.persist_state_with_stats(&state)?;
        let next = Arc::new(next_snapshot);
        self.snapshot.store(next.clone());
        Ok((next, stats))
    }
}
