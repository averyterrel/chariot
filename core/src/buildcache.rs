use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use chariot_util::{
    fs::{FileSystemError, dir_entries, force_rm, make_path},
    lock::{DirLock, LockExclusive, LockShared, block_attempted},
};

use crate::config::package::PackagePlatform;

const CONTENT_SUBDIR: &str = "content";

pub struct BuildCache {
    path: PathBuf,
}

impl BuildCache {
    pub fn get(path: impl AsRef<Path>) -> Result<Self, FileSystemError> {
        make_path(&path)?;

        Ok(Self {
            path: path.as_ref().to_path_buf(),
        })
    }

    fn dir_path(&self, platform: PackagePlatform, arch: &str, name: &str) -> PathBuf {
        self.path.join(format!("{}.{}.{}", platform.to_string(), name, arch))
    }

    pub fn prune(&self, exclude: &HashSet<(PackagePlatform, String)>) -> Result<(), FileSystemError> {
        let _build_cache_lock = DirLock::exclusive(&self.path);

        for entry in dir_entries(&self.path)? {
            if exclude.iter().any(|(platform, name)| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(format!("{}.{}.", platform.to_string(), name).as_str())
            }) {
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
}

pub struct BuildDirectory {
    _dir_lock: DirLock<LockShared>,
    _exclusive_lock: Option<DirLock<LockExclusive>>,
    build_cache: Arc<BuildCache>,
    platform: PackagePlatform,
    arch: String,
    name: String,
}

impl BuildDirectory {
    fn get(build_cache: &Arc<BuildCache>, platform: PackagePlatform, arch: &str, name: &str, exclusive: bool) -> Result<Self, FileSystemError> {
        let _build_cache_lock = DirLock::shared(&build_cache.path);

        let path = build_cache.dir_path(platform, &arch, &name);
        let content_path = path.join(CONTENT_SUBDIR);
        make_path(&content_path)?;

        let exclusive_lock = match exclusive {
            true => Some(DirLock::exclusive(content_path)?),
            false => None,
        };
        let shared_lock = DirLock::shared_noblock(path)?;

        Ok(Self {
            _dir_lock: shared_lock,
            _exclusive_lock: exclusive_lock,
            build_cache: build_cache.clone(),
            platform,
            arch: arch.to_string(),
            name: name.to_string(),
        })
    }

    pub fn get_rw(build_cache: &Arc<BuildCache>, platform: PackagePlatform, arch: &str, name: &str) -> Result<Self, FileSystemError> {
        BuildDirectory::get(build_cache, platform, arch, name, true)
    }

    pub fn get_ro(build_cache: &Arc<BuildCache>, platform: PackagePlatform, arch: &str, name: &str) -> Result<Self, FileSystemError> {
        BuildDirectory::get(build_cache, platform, arch, name, false)
    }

    pub fn path(&self) -> PathBuf {
        self.build_cache.dir_path(self.platform, &self.arch, &self.name).join(CONTENT_SUBDIR)
    }
}
