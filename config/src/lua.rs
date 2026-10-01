use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    fs::{exists, read_to_string, rename},
    path::{Path, PathBuf},
    sync::Arc,
};

use chariot_core::config::{
    Config, Dependencies, GlobalEnvironment,
    package::{Package, PackagePlatform},
    script::{Script, ScriptLanguage},
    source::{Archive, ArchiveCompression, ArchiveKind, GitSource, LocalSource, Source, SourceBase, SourcePrepare},
};
use chariot_util::{
    fs::{FileSystemError, copy_recursive, force_rm, join_soft},
    hash::hash_directory,
};
use mlua::{Error, ErrorContext, Lua, LuaOptions, StdLib, Table, UserData, Value};
use xxhash_rust::xxh3::Xxh3;

use crate::SourceOverride;

pub const EMBEDDED_LUA_FILE_META: &str = include_str!("./lua/meta.lua");
pub const EMBEDDED_LUA_FILE_BUILTINS: &str = include_str!("./lua/builtins.lua");
pub const EMBEDDED_LUA_FILE_HELPERS: &str = include_str!("./lua/helpers.lua");

#[derive(Debug)]
pub struct LuaConfigError {
    cause: Error,
    traceback: Option<String>,
}

impl fmt::Display for LuaConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.cause)?;

        if let Some(traceback) = &self.traceback {
            write!(f, "\nraised from Lua config:")?;
            for line in traceback.trim_start_matches("stack traceback:").trim().lines() {
                write!(f, "\n  {}", line.trim())?;
            }
        }

        Ok(())
    }
}

impl std::error::Error for LuaConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause.source()
    }
}

impl From<Error> for LuaConfigError {
    fn from(err: Error) -> Self {
        let mut cause = err;
        let mut traceback = None;

        while let Error::CallbackError { cause: inner, traceback: tb } = cause {
            traceback = Some(tb);
            cause = (*inner).clone();
        }

        LuaConfigError { cause, traceback }
    }
}

#[derive(Debug)]
struct ChariotAppData {
    sources: Vec<Arc<Source>>,
    packages: Vec<Arc<Package>>,
}

struct SourceRef(Arc<Source>);

impl UserData for SourceRef {}

struct PackageRef(Arc<Package>);

impl UserData for PackageRef {}

fn parse_dependencies_table(table: Table) -> Result<Dependencies, mlua::Error> {
    let mut dependencies = Dependencies::default();
    for pair in table.pairs() {
        let (k, dep): (Value, Value) = pair?;
        match dep {
            Value::UserData(ud) if let Ok(source_ref) = ud.borrow::<SourceRef>() => {
                match k.as_string() {
                    Some(name) => dependencies.sources.insert(name.to_string_lossy(), source_ref.0.clone()),
                    None => return Err(Error::runtime("source dependencies must be named (bound to a key)")),
                };
            }
            Value::UserData(ud) if let Ok(package_ref) = ud.borrow::<PackageRef>() => {
                match package_ref.0.platform {
                    PackagePlatform::Host => dependencies.tools.push(package_ref.0.clone()),
                    PackagePlatform::Target => dependencies.packages.push(package_ref.0.clone()),
                };
            }
            Value::String(pkg) => {
                dependencies.native.insert(pkg.to_string_lossy());
            }
            dep => return Err(Error::runtime(format!("invalid dependency `{}`", dep.to_string()?))),
        }
    }
    Ok(dependencies)
}

fn make_local_source(local_source_storage: &Path, from_path: &Path) -> Result<LocalSource, FileSystemError> {
    let path = from_path.canonicalize().map_err(|err| FileSystemError::Canonicalize {
        path: from_path.to_path_buf(),
        source: err,
    })?;

    let tmp_dir = local_source_storage.join(".tmp");
    force_rm(&tmp_dir)?;
    copy_recursive(path, &tmp_dir)?;

    let mut hasher = Xxh3::new();
    hash_directory(&tmp_dir, &mut hasher)?;
    let hash = hasher.digest128();

    let final_dir = local_source_storage.join(format!("{:x}", hash));

    if exists(&final_dir).map_err(|err| FileSystemError::Exists {
        path: final_dir.clone(),
        source: err,
    })? {
        force_rm(&final_dir)?;
    }

    rename(&tmp_dir, &final_dir).map_err(|err| FileSystemError::Rename {
        from: tmp_dir,
        to: final_dir.clone(),
        source: err,
    })?;

    Ok(LocalSource { path: final_dir, hash })
}

pub fn eval_lua_config(
    path: impl AsRef<Path>,
    project_root: impl AsRef<Path>,
    global_environment: Arc<GlobalEnvironment>,
    options: HashMap<String, String>,
    local_source_storage: impl AsRef<Path>,
    source_overrides: HashMap<(String, PackagePlatform), Vec<SourceOverride>>,
) -> Result<Config, LuaConfigError> {
    let lua = Lua::new_with(StdLib::MATH | StdLib::STRING | StdLib::TABLE | StdLib::PACKAGE, LuaOptions::new())?;

    let package: Table = lua.globals().get("package")?;
    package.set(
        "path",
        format!(
            "{}/?.lua;{}/?/init.lua",
            project_root.as_ref().to_string_lossy(),
            project_root.as_ref().to_string_lossy()
        ),
    )?;

    lua.set_app_data(ChariotAppData {
        sources: Vec::new(),
        packages: Vec::new(),
    });

    let options_table = lua.create_table()?;
    for (k, v) in options {
        options_table.set(k, lua.create_string(v)?)?;
    }

    let source_overrides = Arc::new(source_overrides);

    let chariot_table = lua.create_table()?;
    chariot_table.set("options", options_table)?;
    chariot_table.set("target_prefix", lua.create_string(&global_environment.target_prefix)?)?;
    chariot_table.set("target_arch", lua.create_string(&global_environment.target_arch)?)?;
    chariot_table.set("read_file", {
        let project_root = project_root.as_ref().to_path_buf();
        lua.create_function(move |_, path: PathBuf| {
            let project_root_relative_path = join_soft(&project_root, &path);
            Ok(read_to_string(project_root_relative_path).map_err(|err| Error::ExternalError(Arc::new(err)))?)
        })?
    })?;
    chariot_table.set("def_source", {
        let project_root = project_root.as_ref().to_path_buf();
        let global_environment = global_environment.clone();
        let local_source_storage = local_source_storage.as_ref().to_path_buf();
        lua.create_function(move |l, (base, patches, prepare): (Table, Option<Vec<String>>, Option<Table>)| {
            let base = match base.get::<String>("type").context("`type` must be a string")?.as_str() {
                "archive" => {
                    let url = base.get::<String>("url").context("`url` must be a string")?;
                    let checksum = base.get::<String>("checksum").context("`checksum` must be a string")?;
                    let kind = base.get::<String>("kind").context("`kind` must be a string")?;
                    let compression = base.get::<String>("compression").context("`compression` must be a string")?;

                    let kind = match kind.as_str() {
                        "tar" => ArchiveKind::Tar,
                        _ => return Err(Error::runtime(format!("invalid archive kind `{}`", kind))),
                    };

                    let compression = match compression.as_str() {
                        "gz" => ArchiveCompression::Gzip,
                        "xz" => ArchiveCompression::Xz,
                        "bzip2" => ArchiveCompression::Bzip2,
                        _ => return Err(Error::runtime(format!("invalid archive compression `{}`", compression))),
                    };

                    let archive = Archive {
                        url,
                        checksum,
                        kind,
                        compression,
                    };

                    SourceBase::Archive(archive)
                }
                "git" => {
                    let url = base.get::<String>("url").context("`url` must be a string")?;
                    let revision = base.get::<String>("revision").context("`revision` must be a string")?;

                    SourceBase::Git(GitSource { url, revision })
                }
                "local" => {
                    let path = PathBuf::from(base.get::<String>("path").context("`path` must be a string")?);
                    let path = project_root.join(path);
                    let local_source = make_local_source(&local_source_storage, &path).map_err(|err| Error::ExternalError(Arc::new(err)))?;

                    SourceBase::Local(local_source)
                }
                t => return Err(Error::runtime(format!("invalid base type `{}`", t))),
            };

            let prepare = match prepare {
                Some(prepare) => {
                    let script = prepare.get::<String>("script").context("`script` must be a string")?;
                    let dependencies = parse_dependencies_table(prepare.get::<Table>("dependencies").context("`dependencies` must be a table")?)?;
                    Some(SourcePrepare {
                        global_env: global_environment.clone(),
                        environment_variables: BTreeMap::new(),
                        dependencies,
                        script: Script::new(ScriptLanguage::Bash, script),
                    })
                }
                None => None,
            };

            let source = Arc::new(Source {
                base: base,
                patches: patches.unwrap_or(Vec::new()),
                prepare,
            });

            l.app_data_mut::<ChariotAppData>().unwrap().sources.push(source.clone());

            Ok(SourceRef(source))
        })?
    })?;
    chariot_table.set("def_package", {
        let global_environment = global_environment.clone();
        let source_overrides = source_overrides.clone();
        let local_source_storage = local_source_storage.as_ref().to_path_buf();
        lua.create_function(move |l, pkg: Table| {
            let platform = pkg.get::<String>("platform").context("`platform` must be a string")?;
            let name = pkg.get::<String>("name").context("`name` must be a string")?;
            let version = pkg.get::<String>("version").context("`version` must be a string")?;
            let revision = pkg.get::<usize>("revision").context("`revision` must be a positive integer")?;
            let dependencies = pkg.get::<Table>("dependencies").context("`dependencies` must be a table")?;
            let runtime_dependencies = pkg
                .get::<Table>("runtime_dependencies")
                .context("`runtime_dependencies` must be a table")?;
            let configure = pkg.get::<Option<String>>("configure").context("`configure` must be a string or nil")?;
            let build = pkg.get::<Option<String>>("build").context("`build` must be a string or nil")?;
            let install = pkg.get::<String>("install").context("`install` must be a string")?;

            let platform = match platform.as_str() {
                "host" => PackagePlatform::Host,
                "target" => PackagePlatform::Target,
                _ => return Err(Error::runtime("`platform` must be \"host\" or \"target\"")),
            };

            if l.app_data_ref::<ChariotAppData>()
                .unwrap()
                .packages
                .iter()
                .any(|pkg| pkg.name == name && pkg.platform == platform)
            {
                return Err(Error::runtime(format!("a package with the name `{}` already exists", name)));
            }

            let mut dependencies = parse_dependencies_table(dependencies)?;

            if let Some(overrides) = source_overrides.get(&(name.clone(), platform)) {
                for source_override in overrides {
                    let original = match dependencies.sources.remove_entry(&source_override.name) {
                        None => {
                            return Err(Error::runtime(format!(
                                "source override `{}` does not refer to any source",
                                source_override.name
                            )));
                        }
                        Some((_, v)) => v,
                    };

                    let local_source =
                        make_local_source(&local_source_storage, &source_override.path).map_err(|err| Error::ExternalError(Arc::new(err)))?;

                    let source = Arc::new(Source {
                        base: SourceBase::Local(local_source),
                        patches: match source_override.patched {
                            true => original.patches.clone(),
                            false => Vec::new(),
                        },
                        prepare: match source_override.prepared {
                            true => original.prepare.clone(),
                            false => None,
                        },
                    });

                    l.app_data_mut::<ChariotAppData>().unwrap().sources.push(source.clone());

                    dependencies.sources.insert(name.clone(), source);
                }
            }

            let runtime_dependencies = {
                let mut rdeps = Vec::new();
                for value in runtime_dependencies.sequence_values() {
                    match value? {
                        Value::UserData(ud)
                            if let Ok(package_ref) = ud.borrow::<PackageRef>()
                                && package_ref.0.platform == platform =>
                        {
                            rdeps.push(package_ref.0.clone())
                        }
                        dep => return Err(Error::runtime(format!("invalid runtime dependency `{}`", dep.to_string()?))),
                    }
                }
                rdeps
            };

            let package = Arc::new(Package {
                global_env: global_environment.clone(),
                platform,
                name,
                version,
                revision,
                dependencies,
                runtime_dependencies,
                environment_variables: BTreeMap::new(),
                configure: configure.map(|configure| Script::new(ScriptLanguage::Bash, configure)),
                build: build.map(|build| Script::new(ScriptLanguage::Bash, build)),
                install: Script::new(ScriptLanguage::Bash, install),
            });

            l.app_data_mut::<ChariotAppData>().unwrap().packages.push(package.clone());

            Ok(PackageRef(package))
        })?
    })?;

    let globals = lua.globals();
    globals.set("chariot", chariot_table)?;

    lua.load(EMBEDDED_LUA_FILE_BUILTINS).set_name("=chariot_builtins").exec()?;
    lua.load(EMBEDDED_LUA_FILE_HELPERS).set_name("=chariot_helpers").exec()?;

    lua.load(path.as_ref()).exec()?;

    let app_data = lua.remove_app_data::<ChariotAppData>().unwrap();

    let config = Config {
        global_env: global_environment,
        packages: app_data.packages,
        sources: app_data.sources,
    };

    Ok(config)
}
