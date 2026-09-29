use std::path::PathBuf;

pub mod base;
pub mod lua;

pub const DEFAULT_BASE_CONFIG_PATH: &str = "./chariot_config.toml";
pub const DEFAULT_LUA_CONFIG_PATH: &str = "./chariot.lua";

pub struct SourceOverride {
    pub name: String,
    pub path: PathBuf,
    pub patched: bool,
    pub prepared: bool,
}
