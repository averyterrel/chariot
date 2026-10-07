use std::{
    num::NonZero,
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use chariot_util::fs::FileSystemError;
use nix::{
    errno::Errno,
    fcntl::{OFlag, open},
    sys::stat::Mode,
    unistd::{mkfifo, read, write},
};
use thiserror::Error;

use crate::workdir::{WorkDirectory, WorkDirectoryParent};

#[derive(Debug, Error)]
pub enum CreateJobServerError {
    #[error(transparent)]
    FileSystem(#[from] FileSystemError),

    #[error("failed to create the jobserver fifo")]
    Mkfifo(#[source] Errno),

    #[error("failed to open the jobserver fifo")]
    Open(#[source] Errno),

    #[error("failed to seed the jobserver with tokens")]
    Seed(#[source] Errno),
}

pub struct JobToken<'a> {
    server: &'a JobServer,
}

impl Drop for JobToken<'_> {
    fn drop(&mut self) {
        self.server.release();
    }
}

pub struct JobServer {
    path: PathBuf,
    fd: OwnedFd,
    total: NonZero<usize>,
    implicit_free: AtomicBool,
    _workdir: WorkDirectory,
}

impl JobServer {
    pub fn create(workdir_parent: &Arc<WorkDirectoryParent>, total: NonZero<usize>) -> Result<Self, CreateJobServerError> {
        let workdir = WorkDirectory::create(workdir_parent)?;
        let path = workdir.path().join("jobserver");

        mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).map_err(CreateJobServerError::Mkfifo)?;

        let fd = open(&path, OFlag::O_RDWR | OFlag::O_CLOEXEC, Mode::empty()).map_err(CreateJobServerError::Open)?;

        for _ in 0..total.get() - 1 {
            write(&fd, &[0u8]).map_err(CreateJobServerError::Seed)?;
        }

        Ok(Self {
            path,
            fd,
            total,
            implicit_free: AtomicBool::new(true),
            _workdir: workdir,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn total(&self) -> NonZero<usize> {
        self.total
    }

    pub fn acquire(&self) -> JobToken<'_> {
        if self.implicit_free.swap(false, Ordering::AcqRel) {
            return JobToken { server: self };
        }

        let mut token = [0u8; 1];
        loop {
            match read(&self.fd, &mut token) {
                Ok(1) => break,
                Ok(_) => continue,
                Err(Errno::EINTR) => continue,
                Err(errno) => panic!("jobserver fifo read failed: {errno}"),
            }
        }

        JobToken { server: self }
    }

    fn release(&self) {
        write(&self.fd, &[0u8]).expect("jobserver fifo write failed");
    }
}
