use std::io::{self, Write};

use crate::graph::{TaskId, TaskKind};

pub trait Logger: Write {
    fn captured(&self) -> String;
}

pub struct CapturingLogger<W: Write> {
    inner: W,
    captured: Vec<u8>,
}

impl<W: Write> CapturingLogger<W> {
    pub fn new(inner: W) -> Self {
        Self { inner, captured: Vec::new() }
    }
}

impl<W: Write> Write for CapturingLogger<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.captured.extend_from_slice(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write> Logger for CapturingLogger<W> {
    fn captured(&self) -> String {
        String::from_utf8_lossy(&self.captured).into_owned()
    }
}

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
    Failed { message: String, log: Option<String> },
    Skipped,
}

pub trait Tracer: Send + Sync {
    #[allow(unused_variables)]
    fn register_task(&self, id: TaskId, kind: &TaskKind, referenced_by: &[(TaskId, String)]) {}

    #[allow(unused_variables)]
    fn task_status(&self, id: TaskId, status: TaskStatus) {}

    fn package_step(&self, id: TaskId, step: PackageStep) -> Box<dyn Logger>;

    fn source_step(&self, id: TaskId, step: SourceStep) -> Box<dyn Logger>;

    fn prepare_step(&self, id: TaskId, step: PrepareStep) -> Box<dyn Logger>;
}
