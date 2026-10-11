use std::io::Write;

use crate::graph::{TaskId, TaskKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackageStep {
    Pkgset,
    InstallPackage,
    Configure,
    Build,
    Install,
    Package,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceStep {
    FetchBase,
    Patch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrepareStep {
    Pkgset,
    InstallPackage,
    Prepare,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Started,
    CacheHit,
    Finished,
    Failed { message: String },
    Skipped,
}

pub trait Tracer: Send + Sync {
    #[allow(unused_variables)]
    fn register_task(&self, id: TaskId, kind: &TaskKind) {}

    #[allow(unused_variables)]
    fn task_status(&self, id: TaskId, status: TaskStatus) {}

    fn package_step(&self, id: TaskId, step: PackageStep) -> Box<dyn Write>;

    fn source_step(&self, id: TaskId, step: SourceStep) -> Box<dyn Write>;

    fn prepare_step(&self, id: TaskId, step: PrepareStep) -> Box<dyn Write>;
}
