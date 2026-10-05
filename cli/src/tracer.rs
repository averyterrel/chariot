use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use chariot_core::{
    config::package::PackagePlatform,
    graph::{TaskId, TaskKind},
    tracer::{CapturingLogger, Logger, PackageStep, PrepareStep, SourceStep, TaskStatus, Tracer},
};
use console::{strip_ansi_codes, style};

use crate::terminal::Terminal;

struct TaskState {
    label: String,
    bar: Option<usize>,
}

struct Progress {
    cached: usize,
    completed: usize,
    total: usize,
}

pub struct CliTracer {
    terminal: Arc<Terminal>,
    tasks: Mutex<HashMap<TaskId, TaskState>>,
    progress: Mutex<Progress>,
}

impl CliTracer {
    pub fn new(terminal: Arc<Terminal>) -> Self {
        Self {
            terminal,
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

    fn bar_for(&self, id: TaskId) -> usize {
        let mut tasks = self.tasks.lock().unwrap();
        let state = tasks.get_mut(&id).expect("step logger requested before register_task");
        *state.bar.get_or_insert_with(|| self.terminal.add_bar(state.label.clone()))
    }

    fn step_logger(&self, id: TaskId) -> Box<dyn Logger> {
        Box::new(CapturingLogger::new(self.terminal.get_bar_writer(self.bar_for(id))))
    }
}

impl Tracer for CliTracer {
    fn register_task(&self, id: TaskId, kind: &TaskKind) {
        let mut tasks = self.tasks.lock().unwrap();
        tasks.insert(
            id,
            TaskState {
                label: Self::label_for(kind),
                bar: None,
            },
        );
        drop(tasks);
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
                return;
            }
            TaskStatus::CacheHit => {
                self.advance_progress(0, 1, 1);
            }
            TaskStatus::Failed { message, log } => {
                self.terminal
                    .println(style(format!("* failed: {}: {}", state.label, message)).red().for_stderr().to_string());

                if let Some(log) = log
                    && !log.trim().is_empty()
                {
                    let line = "-".repeat(10);
                    self.terminal.println(
                        style(format!("{line} start log {line}\n{}\n{line} end log {line}", strip_ansi_codes(&log)))
                            .for_stderr()
                            .to_string(),
                    );
                }
            }
            TaskStatus::Skipped => {
                self.terminal
                    .println(style(format!("* skipped: {}", state.label)).dim().yellow().for_stderr().to_string());
            }
            TaskStatus::Finished => {
                self.terminal
                    .println(style(format!("* completed: {}", state.label)).dim().green().for_stderr().to_string());

                self.advance_progress(0, 1, 0);
            }
        }

        if let Some(bar) = state.bar.take() {
            self.terminal.remove_bar(bar);
        }
    }

    fn package_step(&self, id: TaskId, _step: PackageStep) -> Box<dyn Logger> {
        self.step_logger(id)
    }

    fn source_step(&self, id: TaskId, _step: SourceStep) -> Box<dyn Logger> {
        self.step_logger(id)
    }

    fn prepare_step(&self, id: TaskId, _step: PrepareStep) -> Box<dyn Logger> {
        self.step_logger(id)
    }
}
