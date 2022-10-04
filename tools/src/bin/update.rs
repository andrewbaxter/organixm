use anyhow::{anyhow, Result};
use askama::Template;
use chrono::Duration;
use slog::Logger;
use sloggers::{
    terminal::{Destination, TerminalLoggerBuilder},
    types::Severity,
    Build,
};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::{exit, Command},
    str::FromStr,
};
use tools::mount_boot;
use tools::{
    current_meta, ext_info, file_digest, find_root_parts, has_internet_gw, int_err, int_info,
    retry, version_bucket, ExternalMeta, InternalMeta, SimpleCommand,
};
use zstd::stream::raw::Decoder;
use zstd::stream::zio::Writer;

#[derive(Template)]
#[template(path = "grub_two.conf", escape = "none")]
struct GrubTemplate<'a> {
    new: &'a InternalMeta,
    current: &'a InternalMeta,
}

fn main_inner(log: Logger) -> Result<()> {
    let current = current_meta()?;

    // Wait for internet
    retry(&log, Duration::minutes(10), Duration::seconds(10), || {
        if has_internet_gw()? {
            return Ok(());
        }
        return Err(anyhow!("No default route found yet"));
    })?;

    // Get info on candidate version
    let bucket = version_bucket(&current)?;
    let new: ExternalMeta = serde_json::from_slice(
        bucket
            .get_object(format!("{}.meta", current.object_path))
            .map_err(|e| anyhow!("Failed to download meta for new version").context(e))?
            .bytes(),
    )
    .map_err(|e| anyhow!("Failed to parse meta json for new version").context(e))?;
    if current.uuid == new.internal.uuid {
        ext_info!(
            log,
            "Latest version on file server matches currently booted version",
            uuid = &new.internal.uuid
        );
        return Ok(());
    }

    // Identify current and alt root partitions
    let mut found_current = false;
    let mut found_other_part = None;
    let (root_disk, root_parts) = find_root_parts(&log)?;
    for part in root_parts {
        if let Some(_) = part.mountpoint {
            found_current = true;
        } else {
            found_other_part = Some(PathBuf::from_str(&part.path)?);
            let other_digest = file_digest(Path::new(&part.path), new.size)?;
            if other_digest == new.sha256 {
                ext_info!(log, "Digest of alternate partition matches new digest, must have fallen back. Aborting", digest=&new.sha256);
                return Ok(());
            }
        }
    }
    if !found_current {
        return Err(anyhow!("Unable to find mounted root device"));
    }
    let other_path =
        found_other_part.ok_or_else(|| anyhow!("Unable to find alternate root device"))?;

    // Install + check more things
    bucket.get_object_to_writer(
        &new.internal.object_path,
        &mut Writer::new(
            &mut BufWriter::new(&mut File::create(&other_path)?),
            Decoder::new()?,
        ),
    )?;
    let download_digest = file_digest(&other_path, new.size)?;
    if download_digest != new.sha256 {
        return Err(anyhow!(
            "Downloaded digest {} doesn't match reported digest on server {}",
            download_digest,
            new.sha256
        ));
    }

    // Update the grub
    {
        let _mount = mount_boot(log.clone())?;
        File::create("/boot/grub/grub.cfg")?.write_all(
            GrubTemplate {
                current: &current,
                new: &new.internal,
            }
            .render()
            .unwrap()
            .as_ref(),
        )?;
        if let Err(e) = Command::new("grub-install").arg(root_disk.path).run() {
            return Err(anyhow!("grub-install failed").context(e));
        }
    }

    // Reboot into new version
    int_info!(log, "Grub installed successfully, rebooting in 15s");
    std::thread::sleep(Duration::seconds(15).to_std().unwrap());
    Command::new("reboot").run()?;
    Ok(()) // dead code
}

fn main() {
    fn main0() -> bool {
        let mut builder = TerminalLoggerBuilder::new();
        builder.level(Severity::Debug);
        builder.destination(Destination::Stderr);
        let root_log = builder.build().unwrap();
        match main_inner(root_log.clone()) {
            Ok(_) => {
                int_info!(root_log, "Done.");
                return true;
            }
            Err(e) => {
                int_err!(root_log, "Exiting with error", err = format!("{:?}", e));
                return false;
            }
        };
    }
    if !main0() {
        exit(1);
    }
}
