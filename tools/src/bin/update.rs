use anyhow::{anyhow, Context, Result};
use askama::Template;
use chrono::Duration;
use sha2::{Digest, Sha256};
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
    current_meta, ec, err, file_digest, find_root_parts, has_internet_gw, info, retry,
    version_bucket, ExternalMeta, InternalMeta, ProxyWrite, SimpleCommand,
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
    ec!(
        (
            "Updating image from {}/{}",
            current.bucket,
            current.object_path
        ),
        {
            // Wait for internet
            retry(&log, Duration::minutes(10), Duration::seconds(10), || {
                if has_internet_gw()? {
                    return Ok(());
                }
                return Err(anyhow!("No default route found yet"));
            })?;

            // Get info on candidate version
            let bucket = version_bucket(&current)?;

            let new: ExternalMeta = ec!(
                ("Fetching new version meta"),
                Ok(serde_json::from_slice(
                    bucket
                        .get_object(format!("{}.meta", current.object_path))
                        .context("Failed to download meta for new version")?
                        .bytes(),
                )?)
            )?;
            if current.uuid == new.internal.uuid {
                info!(
                    log,
                    "Latest version on file server matches currently booted version",
                    uuid = &new.internal.uuid
                );
                return Ok(());
            }
            info!(
                log,
                "A new version was found, proceeding with update",
                current = &current.uuid,
                new = &new.internal.uuid
            );

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
                        info!(log, "Digest of alternate partition matches new digest, must have fallen back. Aborting", digest=&new.sha256);
                        return Ok(());
                    }
                    info!(
                        log,
                        "Replacing alternate partition with sha",
                        sha = other_digest
                    );
                }
            }
            if !found_current {
                return Err(anyhow!("Unable to find mounted root device"));
            }
            let other_path =
                found_other_part.ok_or_else(|| anyhow!("Unable to find alternate root device"))?;

            // Install + check more things
            info!(log, "Downloading new image");
            let mut digest = Sha256::new();
            ec!(
                ("Downloading new image to {}", other_path.to_string_lossy()),
                {
                    let mut proxy = ProxyWrite {
                        a: &mut digest,
                        b: &mut File::create(&other_path)
                            .context("Failed to open {} for writing")?,
                    };
                    let mut buf_writer = BufWriter::new(&mut proxy);
                    let mut writer = Writer::new(&mut buf_writer, Decoder::new().unwrap());
                    bucket
                        .get_object_to_writer(&new.internal.object_path, &mut writer)
                        .context("Error downloading image")?;
                    writer.finish().context("Failed to flush/finish output")?;
                    Ok(())
                }
            )?;
            let download_digest = format!("{:x}", digest.finalize());
            if download_digest != new.sha256 {
                return Err(anyhow!(
                    "Downloaded digest {} doesn't match reported digest on server {}",
                    download_digest,
                    new.sha256
                ));
            }

            // Update the grub
            info!(log, "Updating grub");
            let grub_cfg_path = "/boot/grub/grub.cfg";
            ec!(
                (
                    "Updating grub on {} with config {}",
                    &root_disk.path,
                    &grub_cfg_path
                ),
                {
                    let _mount = mount_boot(log.clone())?;
                    File::create(grub_cfg_path)
                        .context("Failed to open grub config for writing")?
                        .write_all(
                            GrubTemplate {
                                current: &current,
                                new: &new.internal,
                            }
                            .render()
                            .unwrap()
                            .as_ref(),
                        )
                        .context("Failed to write grub file contents")?;
                    Command::new("grub-install")
                        .arg("--target=i386-pc")
                        .arg(&root_disk.path)
                        .run()?;
                    Ok(())
                }
            )?;

            // Reboot into new version
            info!(log, "Grub installed successfully, rebooting in 15s");
            std::thread::sleep(Duration::seconds(15).to_std().unwrap());
            Command::new("reboot").run()?;
            Ok(()) // dead code
        }
    )
}

fn main() {
    fn main0() -> bool {
        let mut builder = TerminalLoggerBuilder::new();
        builder.level(Severity::Debug);
        builder.destination(Destination::Stderr);
        let root_log = builder.build().unwrap();
        match main_inner(root_log.clone()) {
            Ok(_) => {
                info!(root_log, "Done");
                return true;
            }
            Err(e) => {
                err!(root_log, "Exiting with error", err = format!("{:?}", e));
                return false;
            }
        };
    }
    if !main0() {
        exit(1);
    }
}
