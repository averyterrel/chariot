use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use chariot_core::graph::TaskKind;
use chariot_util::{
    fs::{FileSystemError, dir_entries, force_rm, force_rm_contents, make_path},
    lock::{DirLock, LockExclusive, block_attempted},
};

pub struct TaskLog {
    _lock: DirLock<LockExclusive>,
    path: PathBuf,
}

impl TaskLog {
    pub fn create(logs_path: impl AsRef<Path>, category: &str, hash: u128) -> Result<Option<Self>, FileSystemError> {
        let path = task_log_path(logs_path, category, hash);
        make_path(&path)?;

        let lock = match DirLock::exclusive_noblock(&path) {
            result if block_attempted(&result) => return Ok(None),
            result => result,
        }?;

        force_rm_contents(&path, None)?;

        Ok(Some(Self { _lock: lock, path }))
    }

    pub fn open_step(&self, step: &str) -> Result<File, FileSystemError> {
        let path = self.step_path(step);
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .map_err(|err| FileSystemError::CreateFile { path, source: err })
    }

    pub fn step_path(&self, step: &str) -> PathBuf {
        self.path.join(format!("{}.log", step))
    }
}

pub fn prune_logs(logs_path: impl AsRef<Path>, exclude: &HashSet<(String, u128)>) -> Result<(), FileSystemError> {
    make_path(&logs_path)?;

    for entry in dir_entries(&logs_path)? {
        if exclude
            .iter()
            .any(|(category, hash)| entry.file_name().eq(task_log_name(category, *hash).as_str()))
        {
            continue;
        }

        let _lock = match DirLock::exclusive_noblock(entry.path()) {
            result if block_attempted(&result) => continue,
            result => result,
        }?;

        force_rm(entry.path())?;
    }

    Ok(())
}

pub fn task_log_path(logs_path: impl AsRef<Path>, category: &str, hash: u128) -> PathBuf {
    logs_path.as_ref().join(task_log_name(category, hash))
}

pub fn task_log_ref(kind: &TaskKind) -> (&'static str, u128) {
    match kind {
        TaskKind::Package { package, .. } => ("pkg", package.get_package_hash()),
        TaskKind::Source { source } => (
            "source.prepare",
            source.get_prepare_hash(source.get_prepare_base_hash(source.get_patch_hash(source.get_base_hash()))),
        ),
    }
}

fn task_log_name(category: &str, hash: u128) -> String {
    format!("{}-{:x}", category, hash)
}
