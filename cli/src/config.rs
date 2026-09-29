use std::{
    collections::HashMap,
    fs::{exists, read_to_string},
    iter,
    path::{Path, PathBuf},
};

use anyhow::Result;
use chariot_config::SourceOverride;
use chariot_core::config::package::PackagePlatform;
use serde::Deserialize;

#[derive(Deserialize, Clone)]
#[serde(untagged)]
pub enum OverrideConfig {
    Simple(PathBuf),
    Detailed {
        path: PathBuf,

        #[serde(default)]
        patch: bool,

        #[serde(default)]
        prepare: bool,
    },
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct PackageConfig {
    pub enable_build_cache: bool,
    pub source_overrides: HashMap<String, OverrideConfig>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct CliConfig {
    pub pkgs: HashMap<String, PackageConfig>,
    pub tools: HashMap<String, PackageConfig>,
}

impl CliConfig {
    pub fn get_source_override_map(&self) -> HashMap<(String, PackagePlatform), Vec<SourceOverride>> {
        fn convert_overrides(configs: &HashMap<String, OverrideConfig>) -> Vec<SourceOverride> {
            configs
                .iter()
                .map(|(name, config)| match config {
                    OverrideConfig::Simple(path) => SourceOverride {
                        name: name.clone(),
                        path: path.clone(),
                        patched: false,
                        prepared: false,
                    },
                    OverrideConfig::Detailed { path, patch, prepare } => SourceOverride {
                        name: name.clone(),
                        path: path.clone(),
                        patched: *patch,
                        prepared: *prepare,
                    },
                })
                .collect()
        }

        iter::chain(
            self.pkgs
                .iter()
                .map(|(name, config)| ((name.clone(), PackagePlatform::Target), convert_overrides(&config.source_overrides))),
            self.tools
                .iter()
                .map(|(name, config)| ((name.clone(), PackagePlatform::Host), convert_overrides(&config.source_overrides))),
        )
        .collect()
    }
}

pub fn parse_cli_config(path: impl AsRef<Path>) -> Result<CliConfig> {
    if !exists(&path)? {
        return Ok(CliConfig::default());
    }

    let data = read_to_string(&path)?;
    let config = toml::from_str::<CliConfig>(&data)?;
    Ok(config)
}
