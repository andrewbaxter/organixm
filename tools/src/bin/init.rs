use std::{
    fs::{create_dir_all, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::{exit, Command},
};

use anyhow::{anyhow, Context, Result};
use askama::Template;
use chrono::Duration;
use clap::Parser;
use serde::{Deserialize, Serialize};
use slog::Logger;
use sloggers::{
    terminal::{Destination, TerminalLoggerBuilder},
    types::Severity,
    Build,
};
use tools::{
    copy_finish, ec, err, find_root_parts, info, lsblk, mount_boot, read_bytes, retry,
    ExternalMeta, InternalMeta, SimpleCommand, BOOT_LABEL, ROOT_LABELS,
};
use zstd::stream::{raw::Decoder, zio::Writer};

#[derive(Template)]
#[template(path = "grub_one.conf", escape = "none")]
struct GrubTemplate<'a> {
    new: &'a InternalMeta,
}

#[derive(Serialize, Deserialize)]
struct InitConfig {
    size: u64,
    version: ExternalMeta,
    version_path: PathBuf,
}

#[derive(Parser, Debug)]
#[clap()]
struct Args {
    pub config_path: PathBuf,
}

fn main_inner(log: Logger) -> Result<()> {
    let args = Args::parse();
    let config: InitConfig = ec!(
        (
            "Error reading config from {}",
            args.config_path.to_string_lossy()
        ),
        Ok(serde_json::from_slice(&read_bytes(&args.config_path)?)?)
    )?;

    let root_disk = match lsblk()?
        .into_iter()
        .filter(|d| d.type_field == "disk")
        .next()
    {
        Some(d) => d,
        None => {
            return Err(anyhow!(
                "When looking for a disk to use as the root disk, couldn't find any disks"
            ));
        }
    };

    // Partition
    {
        let mut c = Command::new("parted");
        c.arg("--script")
            .arg(&root_disk.path)
            .arg("--")
            // Drive config
            .arg("mklabel")
            .arg("gpt");

        let mut part = 0;

        // Grub part
        part += 1;
        let mut off = 1;
        c.arg("mkpart").arg("no-fs");
        c.arg(format!("{}MiB", off));
        off += 1;
        c.arg(format!("{}MiB", off));
        c.arg("set").arg("1").arg("bios_grub").arg("on");

        // Boot files
        part += 1;
        c.arg("mkpart").arg("primary").arg("ext4");
        c.arg(format!("{}MiB", off));
        off += 127;
        c.arg(format!("{}MiB", off));
        c.arg("name").arg(format!("{}", part)).arg(BOOT_LABEL);
        c.arg("align-check").arg("optimal").arg(format!("{}", part));

        // Roots
        for l in ROOT_LABELS {
            part += 1;
            c.arg("mkpart").arg("primary").arg("ext4");
            c.arg(format!("{}MiB", off));
            off += config.size * 1024;
            c.arg(format!("{}MiB", off));
            c.arg("name").arg(format!("{}", part)).arg(l);
            c.arg("align-check").arg("optimal").arg(format!("{}", part));
        }

        // RW
        part += 1;
        c.arg("mkpart").arg("primary").arg("ext4");
        c.arg(format!("{}MiB", off));
        c.arg("-1");
        c.arg("name").arg(format!("{}", part)).arg("rw");
        c.arg("align-check").arg("optimal").arg(format!("{}", part));

        c.run()?;
    }

    let boot_path = Path::new(&format!("/dev/disk/by-partlabel/{}", BOOT_LABEL)).to_path_buf();
    let rw_path = Path::new("/dev/disk/by-partlabel/rw");
    for path in &[rw_path, &boot_path] {
        retry(&log, Duration::minutes(5), Duration::seconds(10), || {
            if path.exists() {
                return Ok(());
            } else {
                return Err(anyhow!(
                    "{} doesn't exist yet, needed for mkfs",
                    path.to_string_lossy()
                ));
            }
        })?;
    }

    Command::new("mkfs.ext4").arg(boot_path).run()?;
    Command::new("mkfs.ext4").arg(rw_path).run()?;

    // Install the first version + grub
    let root_part = find_root_parts(&log)?.1[0].clone();

    ec!(
        (
            "Error writing initial image from {} to {}",
            config.version_path.to_string_lossy(),
            &root_part.path
        ),
        Ok(copy_finish(
            &mut File::open(&config.version_path).context("Unable to open source")?,
            &mut Writer::new(
                &mut BufWriter::new(
                    &mut File::create(Path::new(&root_part.path)).context("Unable to open dest")?
                ),
                Decoder::new().unwrap(),
            ),
        )?)
    )?;

    ec!(("Error installing grub"), {
        create_dir_all("/boot").map_err(|e| anyhow!("Failed to create /boot").context(e))?;
        let _mount = mount_boot(log.clone())?;
        create_dir_all("/boot/grub").context("Failed to ensure /boot/grub/")?;
        File::create("/boot/grub/grub.cfg")
            .context("Unable to open grub.cfg for writing")?
            .write_all(
                GrubTemplate {
                    new: &config.version.internal,
                }
                .render()
                .unwrap()
                .as_ref(),
            )
            .context("Error writing to grub.cfg")?;
        Command::new("grub-install")
            .arg("--target=i386-pc")
            .arg(root_disk.path)
            .run()?;
        Ok(())
    })?;

    Command::new("poweroff").run()?;
    return Ok(()); // dead code
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
