use std::{
    collections::{HashMap, VecDeque},
    num::NonZero,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    thread,
};

use chariot_rootfs::GetPkgSetError;
use chariot_runtime::RuntimeError;
use chariot_util::fs::FileSystemError;
use thiserror::Error;

use crate::{
    CoreContext,
    config::{Dependencies, package::Package, source::Source},
    execenv::CreateExecEnvError,
    graph::{BuildGraph, TaskId, TaskKind},
    package,
    source::{self, archive::ArchiveFetchError, git::GitFetchError},
    store::StoreEntry,
    tracer::{TaskStatus, Tracer},
    xbps::XBPSPackageCreateError,
};

#[derive(Debug, Error)]
pub enum ExecuteError {
    #[error(transparent)]
    ExecEnv(#[from] CreateExecEnvError),

    #[error(transparent)]
    FileSystem(#[from] FileSystemError),

    #[error(transparent)]
    Database(#[from] rusqlite::Error),

    #[error(transparent)]
    Runtime(#[from] RuntimeError),

    #[error("{source}")]
    GetPkgSet { source: GetPkgSetError, log: String },

    #[error("{source}")]
    PackageCreate { source: XBPSPackageCreateError, log: String },

    #[error("{source}")]
    Archive { source: ArchiveFetchError, log: String },

    #[error("{source}")]
    Git { source: GitFetchError, log: String },

    #[error("Patch failed")]
    Patch { log: String },

    #[error("Configure failed with the exit code {}", exit_code)]
    Configure { exit_code: i32, log: String },

    #[error("Build failed with the exit code {}", exit_code)]
    Build { exit_code: i32, log: String },

    #[error("Install failed with the exit code {}", exit_code)]
    Install { exit_code: i32, log: String },

    #[error("Prepare failed")]
    Prepare { log: String },
}

impl ExecuteError {
    pub fn captured_log(&self) -> Option<&str> {
        match self {
            Self::GetPkgSet { log, .. }
            | Self::PackageCreate { log, .. }
            | Self::Archive { log, .. }
            | Self::Git { log, .. }
            | Self::Configure { log, .. }
            | Self::Build { log, .. }
            | Self::Install { log, .. }
            | Self::Patch { log }
            | Self::Prepare { log } => Some(log),
            Self::ExecEnv(err) => err.captured_log(),
            Self::FileSystem(_) | Self::Database(_) | Self::Runtime(_) => None,
        }
    }
}

pub enum TaskOutput {
    Package(StoreEntry),
    Source(Vec<StoreEntry>),
}

impl TaskOutput {
    pub fn paths(&self) -> Vec<PathBuf> {
        match self {
            Self::Package(entry) => vec![entry.path()],
            Self::Source(entries) => entries.iter().map(StoreEntry::path).collect(),
        }
    }
}

pub(crate) enum Outcome {
    CacheHit(TaskOutput),
    Built(TaskOutput),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureMode {
    FailFast,
    KeepGoing,
}

#[derive(Debug, Default)]
pub struct BuildReport {
    pub succeeded: Vec<TaskId>,
    pub failed: Vec<(TaskId, String)>,
    pub skipped: Vec<TaskId>,
}

impl BuildReport {
    pub fn is_success(&self) -> bool {
        self.failed.is_empty() && self.skipped.is_empty()
    }
}

enum Termination {
    Succeeded,
    Failed(String),
    Skipped,
}

struct SchedulerState {
    in_degree: HashMap<TaskId, usize>,
    ready: VecDeque<TaskId>,
    pending_count: usize,
    pending: HashMap<TaskId, bool>,
    aborted: bool,
    report: BuildReport,
}

pub struct BuildManager<'a> {
    ctx: &'a CoreContext,
    graph: BuildGraph,
    tracer: Arc<dyn Tracer>,
    results: Mutex<HashMap<TaskId, TaskOutput>>,
}

impl<'a> BuildManager<'a> {
    pub fn new(ctx: &'a CoreContext, graph: BuildGraph, tracer: Arc<dyn Tracer>) -> Self {
        for id in graph.ids() {
            tracer.register_task(id, &graph.kind(id));
        }

        Self {
            ctx,
            graph,
            tracer,
            results: Mutex::new(HashMap::new()),
        }
    }

    pub fn execute(&self, mode: FailureMode, worker_count: NonZero<usize>) -> BuildReport {
        let n = self.graph.len();

        let in_degree: HashMap<TaskId, usize> = self.graph.ids().map(|id| (id, self.graph.dependencies(id).len())).collect();
        let mut dependents: HashMap<TaskId, Vec<TaskId>> = self.graph.ids().map(|id| (id, Vec::new())).collect();
        for id in self.graph.ids() {
            for &dep in self.graph.dependencies(id) {
                dependents.get_mut(&dep).unwrap().push(id);
            }
        }

        let ready: VecDeque<TaskId> = self.graph.ids().filter(|id| in_degree[id] == 0).collect();

        let state = Mutex::new(SchedulerState {
            in_degree,
            ready,
            pending_count: n,
            pending: self.graph.ids().map(|id| (id, true)).collect(),
            aborted: false,
            report: BuildReport::default(),
        });
        let condvar = Condvar::new();

        thread::scope(|scope| {
            for _ in 0..worker_count.get() {
                scope.spawn(|| self.worker(mode, &state, &dependents, &condvar));
            }
        });

        state.into_inner().unwrap().report
    }

    fn worker(&self, mode: FailureMode, state: &Mutex<SchedulerState>, dependents: &HashMap<TaskId, Vec<TaskId>>, condvar: &Condvar) {
        loop {
            let id = {
                let mut state = state.lock().unwrap();
                loop {
                    if let Some(id) = state.ready.pop_front() {
                        break id;
                    }

                    if state.pending_count == 0 {
                        return;
                    }

                    state = condvar.wait(state).unwrap();
                }
            };

            if mode == FailureMode::FailFast && state.lock().unwrap().aborted {
                self.complete(mode, state, dependents, condvar, id, Termination::Skipped);
                continue;
            }

            self.run_task(mode, state, dependents, condvar, id);
        }
    }

    fn run_task(&self, mode: FailureMode, state: &Mutex<SchedulerState>, dependents: &HashMap<TaskId, Vec<TaskId>>, condvar: &Condvar, id: TaskId) {
        let kind = self.graph.kind(id);

        let result = match &kind {
            TaskKind::Package { package, .. } => self.execute_package(id, package),
            TaskKind::Source { source } => self.execute_source(id, source),
        };

        let termination = match result {
            Ok(outcome) => {
                let (output, status) = match outcome {
                    Outcome::CacheHit(output) => (output, TaskStatus::CacheHit),
                    Outcome::Built(output) => (output, TaskStatus::Finished),
                };
                self.results.lock().unwrap().insert(id, output);
                self.tracer.task_status(id, status);
                Termination::Succeeded
            }
            Err(err) => {
                let message = err.to_string();
                let log = err.captured_log().map(str::to_owned);
                self.tracer.task_status(
                    id,
                    TaskStatus::Failed {
                        message: message.clone(),
                        log,
                    },
                );
                Termination::Failed(message)
            }
        };

        self.complete(mode, state, dependents, condvar, id, termination);
    }

    fn complete(
        &self,
        mode: FailureMode,
        state: &Mutex<SchedulerState>,
        dependents: &HashMap<TaskId, Vec<TaskId>>,
        condvar: &Condvar,
        id: TaskId,
        termination: Termination,
    ) {
        let mut state = state.lock().unwrap();
        if !state.pending[&id] {
            return;
        }
        state.pending.insert(id, false);
        state.pending_count -= 1;

        fn skip_descendants(state: &mut SchedulerState, dependents: &HashMap<TaskId, Vec<TaskId>>, id: TaskId) {
            let mut queue: VecDeque<TaskId> = dependents[&id].iter().copied().collect();
            while let Some(dep) = queue.pop_front() {
                if !state.pending[&dep] {
                    continue;
                }
                state.pending.insert(dep, false);
                state.pending_count -= 1;
                state.report.skipped.push(dep);
                queue.extend(dependents[&dep].iter().copied());
            }
        }

        match termination {
            Termination::Succeeded => {
                state.report.succeeded.push(id);
                for &dependent in &dependents[&id] {
                    if !state.pending[&dependent] {
                        continue;
                    }
                    let remaining = state.in_degree.get_mut(&dependent).unwrap();
                    *remaining -= 1;
                    if *remaining == 0 {
                        state.ready.push_back(dependent);
                    }
                }
            }
            Termination::Failed(message) => {
                state.report.failed.push((id, message));
                if mode == FailureMode::FailFast {
                    state.aborted = true;
                }
                skip_descendants(&mut state, dependents, id);
            }
            Termination::Skipped => {
                state.report.skipped.push(id);
                skip_descendants(&mut state, dependents, id);
            }
        }

        condvar.notify_all();
    }

    pub fn package_install_paths(&self, pkg: &Arc<Package>) -> Vec<PathBuf> {
        let task = self.graph.task_for_package(pkg);
        let ids = self.graph.runtime_dependency_closure(task);
        let results = self.results.lock().unwrap();
        ids.into_iter().flat_map(|id| results[&id].paths()).collect()
    }

    pub fn source_paths(&self, source: &Arc<Source>) -> Vec<PathBuf> {
        let id = self.graph.task_for_source(source);
        let results = self.results.lock().unwrap();
        results[&id].paths()
    }

    fn dependency_paths<'deps>(
        &self,
        dependencies: &'deps Dependencies,
    ) -> (
        Vec<(&'deps Source, Vec<PathBuf>)>,
        Vec<(&'deps Package, Vec<PathBuf>)>,
        Vec<(&'deps Package, Vec<PathBuf>)>,
    ) {
        let sources = dependencies
            .sources
            .iter()
            .map(|source| (source.as_ref(), self.source_paths(source)))
            .collect::<Vec<_>>();
        let target_packages = dependencies
            .packages
            .iter()
            .map(|pkg| (pkg.as_ref(), self.package_install_paths(pkg)))
            .collect::<Vec<_>>();
        let host_tools = dependencies
            .tools
            .iter()
            .map(|pkg| (pkg.as_ref(), self.package_install_paths(pkg)))
            .collect::<Vec<_>>();

        (sources, target_packages, host_tools)
    }

    fn execute_package(&self, id: TaskId, package: &Arc<Package>) -> Result<Outcome, ExecuteError> {
        let (sources, target_packages, host_tools) = self.dependency_paths(&package.dependencies);

        package::build(self.ctx, self.tracer.as_ref(), id, package, &sources, &target_packages, &host_tools)
    }

    fn execute_source(&self, id: TaskId, source: &Arc<Source>) -> Result<Outcome, ExecuteError> {
        let (sources, target_packages, host_tools) = match &source.prepare {
            Some(prepare) => self.dependency_paths(&prepare.dependencies),
            None => (Vec::new(), Vec::new(), Vec::new()),
        };

        source::fetch(self.ctx, self.tracer.as_ref(), id, source, &sources, &target_packages, &host_tools)
    }
}
