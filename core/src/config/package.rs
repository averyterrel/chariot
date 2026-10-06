use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    sync::Arc,
};

use xxhash_rust::xxh3::Xxh3;

use crate::{
    HOST_ARCH, HOST_PREFIX,
    config::{CONFIG_VERSION, Dependencies, GlobalEnvironment, script::Script, source::Source},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackagePlatform {
    Host,
    Target,
}

impl ToString for PackagePlatform {
    fn to_string(&self) -> String {
        match self {
            Self::Host => String::from("host"),
            Self::Target => String::from("target"),
        }
    }
}

#[derive(Debug)]
pub struct Package {
    pub global_env: Arc<GlobalEnvironment>,
    pub platform: PackagePlatform,
    pub name: String,
    pub version: String,
    pub revision: usize,
    pub source: Option<Arc<Source>>,
    pub dependencies: Dependencies,
    pub runtime_dependencies: Vec<Arc<Package>>,
    pub environment_variables: BTreeMap<String, String>,
    pub configure: Option<Script>,
    pub build: Option<Script>,
    pub install: Script,
}

impl Package {
    pub fn get_arch(&self) -> &str {
        match self.platform {
            PackagePlatform::Host => HOST_ARCH,
            PackagePlatform::Target => &self.global_env.target_arch,
        }
    }

    pub fn get_prefix(&self) -> &str {
        match self.platform {
            PackagePlatform::Host => HOST_PREFIX,
            PackagePlatform::Target => &self.global_env.target_prefix,
        }
    }

    /// Content hash, not including dependencies.
    pub fn get_content_base_hash(&self) -> u128 {
        let mut hasher = Xxh3::new();
        CONFIG_VERSION.hash(&mut hasher);
        self.global_env.rootfs_manifest_hash.hash(&mut hasher);
        self.global_env.global_environment_variables.hash(&mut hasher);
        self.global_env.global_native_packages.hash(&mut hasher);
        self.get_arch().hash(&mut hasher);
        self.get_prefix().hash(&mut hasher);
        self.environment_variables.hash(&mut hasher);
        self.dependencies.native.hash(&mut hasher);
        self.configure.hash(&mut hasher);
        self.build.hash(&mut hasher);
        self.install.hash(&mut hasher);
        hasher.digest128()
    }

    /// Full content hash including dependency package hashes.
    pub fn get_content_hash(&self) -> u128 {
        let mut hasher = Xxh3::new();
        self.get_content_base_hash().hash(&mut hasher);
        self.source.hash(&mut hasher);
        self.dependencies.sources.hash(&mut hasher);
        self.dependencies.tools.hash(&mut hasher);
        self.dependencies.packages.hash(&mut hasher);
        hasher.digest128()
    }

    /// Hashes all of the package metadata, not the content.
    pub fn get_package_meta_hash(&self) -> u128 {
        let mut hasher = Xxh3::new();
        self.get_arch().hash(&mut hasher);
        self.name.hash(&mut hasher);
        self.version.hash(&mut hasher);
        self.revision.hash(&mut hasher);
        self.runtime_dependencies.hash(&mut hasher);
        hasher.digest128()
    }

    /// Full package hash including content and meta.
    pub fn get_package_hash(&self) -> u128 {
        let mut hasher = Xxh3::new();
        self.get_content_hash().hash(&mut hasher);
        self.get_package_meta_hash().hash(&mut hasher);
        hasher.digest128()
    }
}

impl Hash for Package {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u128(self.get_package_hash());
    }
}
