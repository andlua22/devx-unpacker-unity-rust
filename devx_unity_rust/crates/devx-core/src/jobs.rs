//! Non-blocking background job dispatcher using `crossbeam::channel`.

use crate::catalog::{AssetCatalog, ReferenceGraph, SearchHit};
use crossbeam::channel::{self, Receiver, Sender};
use devx_unity_io::{
    UnityWorkspaceData, extract_raw_bundles, load_deferred_bundle_indices,
    load_deferred_bundles_with_dependencies, load_path,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Events posted by background worker threads back to the UI/host thread.
#[derive(Debug)]
pub enum JobEvent {
    Started {
        id: u64,
        label: String,
    },
    Progress {
        id: u64,
        fraction: f32,
        detail: String,
    },
    WorkspaceLoaded {
        id: u64,
        catalog: Arc<AssetCatalog>,
    },
    ReferenceGraphBuilt {
        id: u64,
        graph: Arc<ReferenceGraph>,
    },
    SearchFinished {
        id: u64,
        matches: Vec<SearchHit>,
    },
    ExportFinished {
        id: u64,
        output_dir: PathBuf,
        files_written: usize,
        errors: Vec<String>,
    },
    RawBundlesExtracted {
        id: u64,
        output_dir: PathBuf,
        files_written: usize,
        total_bytes: u64,
    },
    Failed {
        id: u64,
        error: String,
    },
}

/// Handle to an active background job allowing cancellation.
#[derive(Debug, Clone)]
pub struct JobHandle {
    pub id: u64,
    cancelled: Arc<AtomicBool>,
}

impl JobHandle {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// Coordinates background loading, indexing, search, and export tasks.
pub struct JobManager {
    tx: Sender<JobEvent>,
    rx: Receiver<JobEvent>,
    next_id: AtomicU64,
    repaint: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for JobManager {
    fn default() -> Self {
        Self::new(None)
    }
}

impl JobManager {
    pub fn new(repaint: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let (tx, rx) = channel::unbounded();
        Self {
            tx,
            rx,
            next_id: AtomicU64::new(1),
            repaint,
        }
    }

    pub fn set_repaint_hook(&mut self, hook: Arc<dyn Fn() + Send + Sync>) {
        self.repaint = Some(hook);
    }

    pub fn sender(&self) -> Sender<JobEvent> {
        self.tx.clone()
    }

    pub fn poll_events(&self) -> Vec<JobEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }

    fn emit(&self, event: JobEvent) {
        let _ = self.tx.send(event);
        if let Some(ref hook) = self.repaint {
            hook();
        }
    }

    pub fn alloc_handle(&self, label: impl Into<String>) -> JobHandle {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let handle = JobHandle {
            id,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        self.emit(JobEvent::Started {
            id,
            label: label.into(),
        });
        handle
    }

    pub fn notify_repaint(&self) {
        if let Some(ref hook) = self.repaint {
            hook();
        }
    }

    /// Spawns asynchronous loading and indexing of a file or game folder.
    pub fn spawn_load_path(&self, path: PathBuf) -> JobHandle {
        let handle = self.alloc_handle(format!("Opening {}", path.display()));
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        let id = handle.id;
        let cancelled = handle.cancelled.clone();

        std::thread::spawn(move || {
            let send = |ev: JobEvent| {
                let _ = tx.send(ev);
                if let Some(ref r) = repaint {
                    r();
                }
            };
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            send(JobEvent::Progress {
                id,
                fraction: 0.25,
                detail: "Reading containers...".into(),
            });
            match load_path(&path) {
                Ok(workspace) => {
                    if cancelled.load(Ordering::Relaxed) {
                        return;
                    }
                    send(JobEvent::Progress {
                        id,
                        fraction: 0.65,
                        detail: format!("Indexing {} file(s)...", workspace.files.len()),
                    });
                    let catalog = Arc::new(AssetCatalog::from_workspace(Arc::new(workspace)));
                    send(JobEvent::WorkspaceLoaded { id, catalog });
                }
                Err(err) => {
                    send(JobEvent::Failed {
                        id,
                        error: err.to_string(),
                    });
                }
            }
        });

        handle
    }

    /// Builds the full cross-asset `PPtr` graph on a worker thread.
    pub fn spawn_reference_graph(&self, catalog: Arc<AssetCatalog>) -> JobHandle {
        let handle = self.alloc_handle("Indexing PPtr reference graph");
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        let id = handle.id;
        let cancelled = handle.cancelled.clone();

        std::thread::spawn(move || {
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            let graph = Arc::new(catalog.build_reference_graph());
            let _ = tx.send(JobEvent::ReferenceGraphBuilt { id, graph });
            if let Some(ref r) = repaint {
                r();
            }
        });

        handle
    }

    /// Executes a deep search across metadata and raw payloads on a worker thread.
    pub fn spawn_deep_search(
        &self,
        catalog: Arc<AssetCatalog>,
        query: String,
        search_raw_bytes: bool,
    ) -> JobHandle {
        let handle = self.alloc_handle(format!("Searching '{query}'"));
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        let id = handle.id;
        let cancelled = handle.cancelled.clone();

        std::thread::spawn(move || {
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            let matches = catalog.deep_search(&query, search_raw_bytes);
            let _ = tx.send(JobEvent::SearchFinished { id, matches });
            if let Some(ref r) = repaint {
                r();
            }
        });

        handle
    }

    /// Streams raw `.ab` / `.bundle` / `.unity3d` files onto disk without parsing every entry.
    pub fn spawn_extract_raw_bundles(
        &self,
        source_path: PathBuf,
        output_dir: PathBuf,
        filter: Option<String>,
    ) -> JobHandle {
        let handle = self.alloc_handle(format!("Extracting .ab bundles to {}", output_dir.display()));
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        let id = handle.id;
        let cancelled = handle.cancelled.clone();

        std::thread::spawn(move || {
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            match extract_raw_bundles(&source_path, &output_dir, filter.as_deref()) {
                Ok(report) => {
                    let _ = tx.send(JobEvent::RawBundlesExtracted {
                        id,
                        output_dir,
                        files_written: report.extracted_files,
                        total_bytes: report.total_bytes,
                    });
                }
                Err(err) => {
                    let _ = tx.send(JobEvent::Failed {
                        id,
                        error: err.to_string(),
                    });
                }
            }
            if let Some(ref r) = repaint {
                r();
            }
        });

        handle
    }

    /// Loads selected deferred `.ab` / `.bundle` entries asynchronously (always resolving deps).
    pub fn spawn_load_deferred_bundles(
        &self,
        workspace: UnityWorkspaceData,
        indices: Vec<usize>,
    ) -> JobHandle {
        self.spawn_load_deferred_bundles_opts(workspace, indices, true)
    }

    /// Loads selected deferred `.ab` / `.bundle` entries asynchronously with configurable
    /// transitive `.ab` / `CAB-*` dependency loading.
    pub fn spawn_load_deferred_bundles_opts(
        &self,
        mut workspace: UnityWorkspaceData,
        indices: Vec<usize>,
        include_deps: bool,
    ) -> JobHandle {
        let handle = self.alloc_handle(format!("Loading {} .ab bundle(s)", indices.len()));
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        let id = handle.id;
        let cancelled = handle.cancelled.clone();

        std::thread::spawn(move || {
            let send = |ev: JobEvent| {
                let _ = tx.send(ev);
                if let Some(ref r) = repaint {
                    r();
                }
            };
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            send(JobEvent::Progress {
                id,
                fraction: 0.35,
                detail: format!("Decompressing {} .ab bundle(s)...", indices.len()),
            });
            let res = if include_deps {
                load_deferred_bundles_with_dependencies(&mut workspace, &indices)
                    .map(|(loaded, rep)| (loaded, rep.newly_loaded_bundles))
            } else {
                load_deferred_bundle_indices(&mut workspace, &indices).map(|loaded| (loaded, 0))
            };
            match res {
                Ok((loaded, extra_deps)) => {
                    if cancelled.load(Ordering::Relaxed) {
                        return;
                    }
                    send(JobEvent::Progress {
                        id,
                        fraction: 0.75,
                        detail: if extra_deps > 0 {
                            format!("Indexing {loaded} bundle(s) (+{extra_deps} dependency bundle(s))...")
                        } else {
                            format!("Indexing {loaded} bundle(s)...")
                        },
                    });
                    let catalog = Arc::new(AssetCatalog::from_workspace(Arc::new(workspace)));
                    send(JobEvent::WorkspaceLoaded { id, catalog });
                }
                Err(err) => {
                    send(JobEvent::Failed {
                        id,
                        error: err.to_string(),
                    });
                }
            }
        });

        handle
    }
}
