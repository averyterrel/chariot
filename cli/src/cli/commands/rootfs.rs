use std::{collections::BTreeSet, io::stdout, sync::Arc};

use anyhow::{Context, Result, bail};
use chariot_rootfs::{CachedPkgSet, ManifestFetchSpec, PkgSetState, RootFS};
use log::info;

use crate::cli::args::{PkgsetCommand, RootFSCommand, RootFSOptions};

pub fn run(options: RootFSOptions) -> Result<()> {
    let RootFSOptions {
        rootfs: rootfs_path,
        command,
    } = options;

    if let RootFSCommand::Init { url, version, hash } = command {
        let spec = ManifestFetchSpec { url, version, hash };
        RootFS::init(&rootfs_path, &spec, &mut stdout())?;
        info!("rootfs initialized at {}", rootfs_path.display());
        return Ok(());
    }

    let rootfs = match RootFS::get(&rootfs_path)? {
        Some(r) => r,
        None => bail!("no intact rootfs found at {}", rootfs_path.display()),
    };

    let rootfs = Arc::new(rootfs);

    match command {
        RootFSCommand::Init { .. } => {}
        RootFSCommand::Status => {
            let spec = rootfs.get_manifest_spec();
            info!("path:    {}", rootfs_path.display());
            info!("url:     {}", spec.url);
            info!("version: {}", spec.version);
            info!("hash:    {}", spec.hash);
        }
        RootFSCommand::Pkgset(PkgsetCommand::List) => {
            let pkgsets = rootfs.list_pkgsets()?;
            if pkgsets.is_empty() {
                info!("no cached package sets");
                return Ok(());
            }
            info!("{:<6} {:<12} {:<6} {:<6} {:<12} packages", "id", "state", "base", "depth", "size");
            info!("{}", "-".repeat(60));
            for ps in pkgsets {
                let state = match ps.state {
                    PkgSetState::Unknown => "unknown",
                    PkgSetState::Cached => "cached",
                    PkgSetState::Deduplicated => "deduped",
                };
                info!(
                    "{:<6} {:<12} {:<6} {:<6} {:<12} {}",
                    ps.id,
                    state,
                    ps.base.map(|id| id.to_string()).unwrap_or(String::new()),
                    ps.base_depth,
                    format_size(ps.size),
                    ps.packages.iter().map(|str| str.as_ref()).collect::<Vec<_>>().join(", "),
                );
            }
        }
        RootFSCommand::Pkgset(PkgsetCommand::Cache { packages }) => {
            CachedPkgSet::get(&rootfs, &None, &BTreeSet::from_iter(packages.iter()), &mut stdout()).context("failed to get cached package set")?;
        }
        RootFSCommand::Purge => {
            let (total, removed, in_use) = rootfs.prune_pkgsets(|_| true)?;
            info!("purged {removed}/{total} package sets ({in_use} in use, skipped)");
        }
    }

    Ok(())
}

fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = UNITS[0];
    for &u in &UNITS[1..] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = u;
    }
    if unit == "B" { format!("{bytes}B") } else { format!("{value:.1}{unit}") }
}
