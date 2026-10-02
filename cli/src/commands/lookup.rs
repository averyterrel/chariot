use std::{process::exit, sync::Arc};

use anyhow::Result;
use chariot_core::{config::package::PackagePlatform, store::StoreEntry};

use crate::{
    args::LookupOptions,
    build::find_package,
    cache::Cache,
    cli_config::CliConfig,
    config::{ResolvedProfile, resolve_profile},
};

pub fn run(lookup_opts: LookupOptions, local_config: &CliConfig) -> Result<()> {
    let platform = if lookup_opts.tool {
        PackagePlatform::Host
    } else {
        PackagePlatform::Target
    };

    let cache = Cache::get(&lookup_opts.config_opts.cache)?;
    let ResolvedProfile { config, .. } = resolve_profile(&cache, lookup_opts.config_opts, local_config)?;

    let pkg = find_package(&config, platform, &lookup_opts.name)?;

    let store = Arc::new(cache.open_store()?);
    let ledger = cache.open_ledger()?;

    let store_entry = match ledger.lookup("pkg", pkg.get_package_hash())? {
        Some(effective_hash) => StoreEntry::get(&store, "pkg", effective_hash)?,
        None => None,
    };

    match store_entry {
        Some(entry) => {
            println!("{}", entry.path().display());
            Ok(())
        }
        None => exit(1),
    }
}
