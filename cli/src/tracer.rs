use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write, stderr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use chariot_core::{
    config::package::PackagePlatform,
    graph::{TaskId, TaskKind},
    tracer::{PackageStep, PrepareStep, SourceStep, TaskStatus, Tracer},
};
use console::{strip_ansi_codes, style};
use log::{info, warn};

use crate::{
    cache::tasklog::{TaskLog, task_log_path, task_log_ref},
    terminal::Terminal,
};

const FAILURE_LOG_MAX_LINES: usize = 100;
const FAILURE_LOG_MAX_BYTES: u64 = 256 * 1024;

struct TaskLogState {
    log: TaskLog,
    step: Option<String>,
}

struct TaskLogs {
    logs_path: PathBuf,
    logs: Mutex<HashMap<TaskId, TaskLogState>>,
}

impl TaskLogs {
    fn new(logs_path: PathBuf) -> Self {
        Self {
            logs_path,
            logs: Mutex::new(HashMap::new()),
        }
    }

    fn open(&self, id: TaskId, category: &str, hash: u128) {
        match TaskLog::create(&self.logs_path, category, hash) {
            Ok(Some(log)) => {
                self.logs.lock().unwrap().insert(id, TaskLogState { log, step: None });
            }
            Ok(None) => warn!(
                "Logs in `{}` are written by another build already",
                task_log_path(&self.logs_path, category, hash).display()
            ),
            Err(err) => warn!("Failed to open logs: {}", err),
        }
    }

    fn begin_step(&self, id: TaskId, step: &str) -> Option<File> {
        let mut logs = self.logs.lock().unwrap();
        let state = logs.get_mut(&id)?;
        state.step = Some(step.to_string());

        match state.log.open_step(step) {
            Ok(file) => Some(file),
            Err(err) => {
                warn!("Failed to open log file of step `{}`: {}", step, err);
                None
            }
        }
    }

    fn last_step_path(&self, id: TaskId) -> Option<PathBuf> {
        let logs = self.logs.lock().unwrap();
        let state = logs.get(&id)?;
        Some(state.log.step_path(state.step.as_deref()?))
    }

    fn close(&self, id: TaskId) {
        self.logs.lock().unwrap().remove(&id);
    }
}

struct StepWriter {
    inner: Box<dyn Write>,
    log: Option<File>,
}

impl Write for StepWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;

        if let Some(log) = &mut self.log {
            let _ = log.write_all(&buf[..written]);
        }

        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(log) = &mut self.log {
            let _ = log.flush();
        }

        self.inner.flush()
    }
}

struct TaskState {
    label: String,
    log_category: &'static str,
    log_hash: u128,
    bar: Option<usize>,
}

struct Progress {
    cached: usize,
    completed: usize,
    total: usize,
}

pub struct CliTracer {
    terminal: Arc<Terminal>,
    logs: TaskLogs,
    tasks: Mutex<HashMap<TaskId, TaskState>>,
    progress: Mutex<Progress>,
}

impl CliTracer {
    pub fn new(terminal: Arc<Terminal>, logs_path: impl AsRef<Path>) -> Self {
        Self {
            terminal,
            logs: TaskLogs::new(logs_path.as_ref().to_path_buf()),
            tasks: Mutex::new(HashMap::new()),
            progress: Mutex::new(Progress {
                completed: 0,
                cached: 0,
                total: 0,
            }),
        }
    }

    fn advance_progress(&self, total_delta: usize, completed_delta: usize, cached_delta: usize) {
        let mut progress = self.progress.lock().unwrap();
        progress.total += total_delta;
        progress.completed += completed_delta;
        progress.cached += cached_delta;
        self.terminal.set_footer(format!(
            "{}/{} tasks done ({} cache hits)",
            progress.completed, progress.total, progress.cached
        ));
    }

    fn bar_for(&self, id: TaskId) -> usize {
        let mut tasks = self.tasks.lock().unwrap();
        let state = tasks.get_mut(&id).expect("step logger requested before register_task");
        *state.bar.get_or_insert_with(|| self.terminal.add_bar(state.label.clone()))
    }

    fn step_logger(&self, id: TaskId, step: &str) -> Box<dyn Write> {
        Box::new(StepWriter {
            inner: Box::new(self.terminal.get_bar_writer(self.bar_for(id))),
            log: self.logs.begin_step(id, step),
        })
    }
}

impl Tracer for CliTracer {
    fn register_task(&self, id: TaskId, kind: &TaskKind) {
        let (log_category, log_hash) = task_log_ref(kind);

        let mut tasks = self.tasks.lock().unwrap();
        tasks.insert(
            id,
            TaskState {
                label: label_for(kind),
                log_category,
                log_hash,
                bar: None,
            },
        );
        self.advance_progress(1, 0, 0);
    }

    fn task_status(&self, id: TaskId, status: TaskStatus) {
        let mut tasks = self.tasks.lock().unwrap();
        let Some(state) = tasks.get_mut(&id) else { return };

        match status {
            TaskStatus::Started => {
                if state.bar.is_none() {
                    state.bar = Some(self.terminal.add_bar(state.label.clone()));
                }
                self.logs.open(id, state.log_category, state.log_hash);
                return;
            }
            TaskStatus::CacheHit => {
                self.advance_progress(0, 1, 1);
            }
            TaskStatus::Failed { message } => {
                self.terminal
                    .println(style(format!("* failed: {}: {}", state.label, message)).red().for_stderr().to_string());

                if let Some(path) = self.logs.last_step_path(id) {
                    if let Some(tail) = log_tail(&path) {
                        self.terminal.println(style(tail).for_stderr().to_string());
                    }

                    self.terminal
                        .println(style(format!("full log: {}", path.display())).dim().for_stderr().to_string());
                }
            }
            TaskStatus::Skipped => {
                self.terminal
                    .println(style(format!("* skipped: {}", state.label)).dim().yellow().for_stderr().to_string());
            }
            TaskStatus::Finished => {
                self.terminal.println(
                    style(format!(
                        "* completed{}: {}",
                        if let Some(bar) = &state.bar
                            && let Some(elapsed) = self.terminal.get_bar_elapsed(*bar)
                        {
                            format!(" in {}", format_duration(elapsed))
                        } else {
                            String::new()
                        },
                        state.label,
                    ))
                    .dim()
                    .green()
                    .for_stderr()
                    .to_string(),
                );

                self.advance_progress(0, 1, 0);
            }
        }

        self.logs.close(id);

        if let Some(bar) = state.bar.take() {
            self.terminal.remove_bar(bar);
        }
    }

    fn package_step(&self, id: TaskId, step: PackageStep) -> Box<dyn Write> {
        self.step_logger(id, package_step_name(step))
    }

    fn source_step(&self, id: TaskId, step: SourceStep) -> Box<dyn Write> {
        self.step_logger(id, source_step_name(step))
    }

    fn prepare_step(&self, id: TaskId, step: PrepareStep) -> Box<dyn Write> {
        self.step_logger(id, prepare_step_name(step))
    }
}

pub struct SimpleTracer {
    logs: TaskLogs,
    tasks: Mutex<HashMap<TaskId, TaskKind>>,
}

impl SimpleTracer {
    pub fn new(logs_path: impl AsRef<Path>) -> Self {
        Self {
            logs: TaskLogs::new(logs_path.as_ref().to_path_buf()),
            tasks: Mutex::new(HashMap::new()),
        }
    }

    fn step_logger(&self, id: TaskId, step: &str) -> Box<dyn Write> {
        Box::new(StepWriter {
            inner: Box::new(stderr()),
            log: self.logs.begin_step(id, step),
        })
    }
}

impl Tracer for SimpleTracer {
    fn register_task(&self, id: TaskId, kind: &TaskKind) {
        self.tasks.lock().unwrap().insert(id, kind.clone());
    }

    fn task_status(&self, id: TaskId, status: TaskStatus) {
        let mut tasks = self.tasks.lock().unwrap();
        let Some(kind) = tasks.get_mut(&id) else { return };

        match status {
            TaskStatus::Started => {
                info!("Task {} started", label_for(kind));
                let (category, hash) = task_log_ref(kind);
                self.logs.open(id, category, hash);
                return;
            }
            TaskStatus::CacheHit => info!("Task {} done (cache hit)", label_for(kind)),
            TaskStatus::Finished => info!("Task {} done", label_for(kind)),
            TaskStatus::Skipped => info!("Task {} skipped", label_for(kind)),
            TaskStatus::Failed { message } => {
                info!("Task {} failed ({})", label_for(kind), message);

                if let Some(path) = self.logs.last_step_path(id) {
                    if let Some(tail) = log_tail(&path) {
                        eprintln!("{}", tail);
                    }

                    info!("Full log of task {} is at `{}`", label_for(kind), path.display());
                }
            }
        }

        self.logs.close(id);
    }

    fn package_step(&self, id: TaskId, step: PackageStep) -> Box<dyn Write> {
        self.step_logger(id, package_step_name(step))
    }

    fn source_step(&self, id: TaskId, step: SourceStep) -> Box<dyn Write> {
        self.step_logger(id, source_step_name(step))
    }

    fn prepare_step(&self, id: TaskId, step: PrepareStep) -> Box<dyn Write> {
        self.step_logger(id, prepare_step_name(step))
    }
}

fn log_tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();

    let from = length.saturating_sub(FAILURE_LOG_MAX_BYTES);
    file.seek(SeekFrom::Start(from)).ok()?;

    let mut data = Vec::new();
    file.read_to_end(&mut data).ok()?;

    let text = String::from_utf8_lossy(&data);
    let text = strip_ansi_codes(&text);

    let mut lines = text.lines().collect::<Vec<_>>();
    if from > 0 && !lines.is_empty() {
        lines.remove(0);
    }

    let skipped = lines.len().saturating_sub(FAILURE_LOG_MAX_LINES);
    let tail = &lines[skipped..];
    if tail.iter().all(|line| line.trim().is_empty()) {
        return None;
    }

    let rule = "-".repeat(10);
    let heading = match from > 0 || skipped > 0 {
        true => format!("last {} lines of log", tail.len()),
        false => String::from("log"),
    };

    Some(format!("{rule} {heading} {rule}\n{}\n{rule} end of log {rule}", tail.join("\n")))
}

fn package_step_name(step: PackageStep) -> &'static str {
    match step {
        PackageStep::Pkgset => "pkgset",
        PackageStep::InstallPackage => "install-dependencies",
        PackageStep::Configure => "configure",
        PackageStep::Build => "build",
        PackageStep::Install => "install",
        PackageStep::Package => "package",
    }
}

fn source_step_name(step: SourceStep) -> &'static str {
    match step {
        SourceStep::FetchBase => "fetch",
        SourceStep::Patch => "patch",
    }
}

fn prepare_step_name(step: PrepareStep) -> &'static str {
    match step {
        PrepareStep::Pkgset => "prepare-pkgset",
        PrepareStep::InstallPackage => "prepare-install-dependencies",
        PrepareStep::Prepare => "prepare",
    }
}

fn label_for(kind: &TaskKind) -> String {
    match kind {
        TaskKind::Package { package, .. } => format!(
            "{} {}",
            match package.platform {
                PackagePlatform::Host => "tool",
                PackagePlatform::Target => "package",
            },
            package.name
        ),
        TaskKind::Source { source } => format!("source {}", source.name),
    }
}

fn format_duration(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);

    match (h, m) {
        (0, 0) => format!("{s}s"),
        (0, _) => format!("{m}m {s}s"),
        _ => format!("{h}h {m}m {s}s"),
    }
}
