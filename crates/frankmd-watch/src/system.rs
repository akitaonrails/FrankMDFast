//! Re-arming watchers for paths outside the vault (Omarchy theme, language
//! file, fontconfig) that may not exist yet or get replaced wholesale.
//!
//! Each watcher targets a directory. While it exists it is watched
//! directly; otherwise the nearest existing ancestor is watched
//! (non-recursively) and the watch moves down as directories appear (OM-003).
//! The arming state is re-checked on every poll tick, so a directory created
//! between "check" and "watch" is never missed.

use crate::WatchEvent;
use crate::emit::{Emitter, describe};
use crate::vault::POLL;
use notify_debouncer_full::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long to wait before retrying after arming failed.
const RETRY_AFTER: Duration = Duration::from_secs(2);

/// One system watcher.
pub(crate) struct Spec {
    /// Thread / error label.
    pub name: &'static str,
    /// Directory to watch.
    pub target: PathBuf,
    /// Watch `target` recursively.
    pub recursive: bool,
    /// Whether a path relative to `target` is interesting.
    pub relevant: Box<dyn Fn(&Path) -> bool + Send>,
    /// What to emit.
    pub event: WatchEvent,
    /// Debounce window.
    pub debounce: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Armed {
    Target,
    Ancestor(PathBuf),
}

struct Active {
    armed: Armed,
    /// `spec.target` with symlinks resolved, as FSEvents reports paths.
    canonical_target: PathBuf,
    rx: Receiver<DebounceEventResult>,
    _debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
}

struct Worker {
    spec: Spec,
    emitter: Arc<Emitter>,
    active: Option<Active>,
    last_failure: Option<Instant>,
}

/// Arm `spec` synchronously (so callers can rely on it being live) and run it.
pub(crate) fn spawn(spec: Spec, emitter: Arc<Emitter>) -> Result<JoinHandle<()>, String> {
    let name = format!("frankmd-watch-{}", spec.name);
    let mut worker = Worker {
        spec,
        emitter,
        active: None,
        last_failure: None,
    };
    worker.ensure_armed();
    std::thread::Builder::new()
        .name(name)
        .spawn(move || worker.run())
        .map_err(|e| format!("watcher thread: {}", e.kind()))
}

/// Where the watch should be right now.
fn desired(target: &Path) -> Option<Armed> {
    if target.is_dir() {
        return Some(Armed::Target);
    }
    target
        .ancestors()
        .skip(1)
        .find(|p| p.is_dir())
        .map(|p| Armed::Ancestor(p.to_path_buf()))
}

impl Worker {
    fn run(mut self) {
        while !self.emitter.is_stopped() {
            let received = match &self.active {
                Some(active) => active.rx.recv_timeout(POLL),
                None => {
                    std::thread::park_timeout(POLL);
                    Err(RecvTimeoutError::Timeout)
                }
            };
            match received {
                Ok(Ok(events)) => {
                    let on_target = matches!(
                        self.active,
                        Some(Active {
                            armed: Armed::Target,
                            ..
                        })
                    );
                    let hit = on_target
                        && events.iter().any(|ev| {
                            ev.need_rescan() || ev.paths.iter().any(|p| self.is_relevant(p))
                        });
                    if hit {
                        self.emitter.emit(self.spec.event.clone());
                    }
                }
                Ok(Err(errors)) => {
                    for err in errors {
                        self.emitter
                            .emit(WatchEvent::Error(describe(self.spec.name, &err)));
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                // The debouncer died; re-arm from scratch.
                Err(RecvTimeoutError::Disconnected) => self.active = None,
            }
            self.ensure_armed();
        }
    }

    fn is_relevant(&self, path: &Path) -> bool {
        let canonical = self.active.as_ref().map(|a| a.canonical_target.as_path());
        let rel = [Some(self.spec.target.as_path()), canonical]
            .into_iter()
            .flatten()
            .find_map(|root| path.strip_prefix(root).ok());
        match rel {
            Some(rel) if rel.as_os_str().is_empty() => false,
            Some(rel) => (self.spec.relevant)(rel),
            None => false,
        }
    }

    /// Move the watch to where it should be; emit when the target appears or
    /// disappears (its contents changed as far as consumers are concerned).
    fn ensure_armed(&mut self) {
        let want = desired(&self.spec.target);
        let have = self.active.as_ref().map(|a| a.armed.clone());
        if want == have {
            return;
        }
        if have.is_none()
            && self
                .last_failure
                .is_some_and(|at| at.elapsed() < RETRY_AFTER)
        {
            return;
        }
        let was_target = have == Some(Armed::Target);
        self.active = None;
        let Some(want) = want else {
            return;
        };
        match self.arm(&want) {
            Ok(active) => {
                self.last_failure = None;
                self.active = Some(active);
            }
            Err(message) => {
                if self.last_failure.is_none() {
                    self.emitter.emit(WatchEvent::Error(message));
                }
                self.last_failure = Some(Instant::now());
            }
        }
        // Only a transition counts, not the initial arming.
        let is_target = want == Armed::Target;
        if have.is_some() && was_target != is_target {
            self.emitter.emit(self.spec.event.clone());
        }
    }

    fn arm(&self, armed: &Armed) -> Result<Active, String> {
        let (path, mode) = match armed {
            Armed::Target if self.spec.recursive => (&self.spec.target, RecursiveMode::Recursive),
            Armed::Target => (&self.spec.target, RecursiveMode::NonRecursive),
            Armed::Ancestor(dir) => (dir, RecursiveMode::NonRecursive),
        };
        let err = |e| describe(self.spec.name, &e);
        let (tx, rx) = mpsc::channel();
        let mut debouncer = new_debouncer(self.spec.debounce, None, tx).map_err(err)?;
        debouncer.watch(path, mode).map_err(err)?;
        Ok(Active {
            armed: armed.clone(),
            canonical_target: self
                .spec
                .target
                .canonicalize()
                .unwrap_or_else(|_| self.spec.target.clone()),
            rx,
            _debouncer: debouncer,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn desired_falls_back_to_nearest_existing_ancestor() {
        let tmp = std::env::temp_dir();
        let target = tmp.join("frankmd-watch-missing-xyz/a/b");
        assert_eq!(desired(&target), Some(Armed::Ancestor(tmp.clone())));
        assert_eq!(desired(&tmp), Some(Armed::Target));
    }
}
