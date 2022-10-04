use anyhow::{anyhow, Result};
use chrono::{Duration, Utc};
use hhmmss::Hhmmss;
use s3::{creds::Credentials, Bucket};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use slog::Logger;
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
    thread,
};

pub mod slogextra;

pub const BOOT_LABEL: &'static str = "boot";
pub const ROOT_LABELS: [&'static str; 2] = ["organixm-a", "brone-b"];

pub fn read_bytes(p: &Path) -> Result<Vec<u8>> {
    let mut buf = vec![];
    File::open(p)
        .map_err(|e| anyhow!("Error opening {} to read", p.to_string_lossy()).context(e))?
        .read_to_end(&mut buf)
        .map_err(|e| anyhow!("Error while reading {}", p.to_string_lossy()).context(e))?;
    return Ok(buf);
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IpRoute {
    // "default" or cidr
    pub dst: String,
    pub gateway: Option<String>,
    // rel dev node
    pub dev: String,
}

pub fn has_internet_gw() -> Result<bool> {
    let output = Command::new("ip")
        .arg("--json")
        .arg("route")
        .arg("show")
        .output()
        .map_err(|e| anyhow!("Failed to run ip route show").context(e))?;
    let parsed = serde_json::from_slice::<Vec<IpRoute>>(&output.stdout)
        .map_err(|e| anyhow!("Failed to parse ip route show output:\n{:?}", &output).context(e))?;
    for r in parsed {
        if r.dst != "default" {
            continue;
        }
        return Ok(true);
    }
    return Ok(false);
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsblkRoot {
    pub blockdevices: Vec<LsblkDevice>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsblkDevice {
    pub path: String,
    pub size: i64,
    #[serde(rename = "type")]
    pub type_field: String,
    pub mountpoint: Option<String>,
    pub partlabel: Option<String>,
    #[serde(default)]
    pub children: Vec<LsblkDevice>,
}

pub fn lsblk() -> Result<Vec<LsblkDevice>> {
    let output = Command::new("lsblk")
        .arg("-n")
        .arg("-b")
        .arg("-J")
        .arg("-o")
        .arg("SIZE,TYPE,PATH,MOUNTPOINT,PARTLABEL")
        .arg("-T")
        .output()
        .map_err(|e| anyhow!("Failed to run lsblk").context(e))?;
    Ok(serde_json::from_slice::<LsblkRoot>(&output.stdout)
        .map_err(|e| anyhow!("Failed to parse lsblk output:\n{:?}", &output).context(e))?
        .blockdevices)
}

pub fn find_root_parts(log: &Logger) -> Result<(LsblkDevice, [LsblkDevice; 2])> {
    let mut out = vec![];
    let lsblk_res = lsblk()?;
    for lsblk_parent in lsblk_res {
        if lsblk_parent.type_field != "disk" {
            continue;
        }
        for part in &lsblk_parent.children {
            let label = match &part.partlabel {
                Some(l) => l,
                None => {
                    ext_trace!(log, "Device has no gpt label, skipping", dev = &part.path);
                    continue;
                }
            };
            if !ROOT_LABELS.iter().any(|l| *l == label) {
                ext_trace!(
                    log,
                    "Device has unknown gpt label, skipping",
                    dev = &part.path,
                    label = &label
                );
                continue;
            }
            out.push(part.clone());
        }
        if out.len() == 2 {
            return Ok((lsblk_parent, [out.remove(0), out.remove(0)]));
        }
    }
    return Err(anyhow!(
        "Expected to find {} root partitions, but found {} (or they were on separate disks)",
        ROOT_LABELS.len(),
        out.len()
    ));
}

pub struct Mount {
    log: Logger,
    dest: PathBuf,
}

impl Mount {
    pub fn new(log: Logger, source: &Path, dest: &Path) -> Result<Mount> {
        let mount_out = Command::new("mount")
            .arg(source.as_os_str())
            .arg(dest.as_os_str())
            .output()
            .map_err(|e| {
                anyhow!(
                    "Failed to run mount of {} to {}",
                    source.to_string_lossy(),
                    dest.to_string_lossy()
                )
                .context(e)
            })?;
        if !mount_out.status.success() {
            return Err(anyhow!(
                "Mount {} to {} failed: {:?}",
                source.to_string_lossy(),
                dest.to_string_lossy(),
                mount_out
            ));
        }
        Ok(Mount {
            log: log,
            dest: dest.to_path_buf(),
        })
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        if let Err(e) = Command::new("umount").arg(self.dest.as_os_str()).run() {
            ext_warn!(
                self.log,
                "Failed to unmount",
                dest = self.dest.to_string_lossy().to_string(),
                err = format!("{:?}", e)
            );
            return;
        }
    }
}

pub fn mount_boot(log: Logger) -> Result<Mount> {
    Mount::new(
        log.clone(),
        Path::new(&format!("/dev/disk/by-partlabel/{}", BOOT_LABEL)),
        Path::new("/boot"),
    )
}

#[derive(Deserialize, Serialize)]
pub struct InternalMeta {
    // AWS region or custom endpoint
    pub region: String,
    pub bucket: String,
    pub object_path: String,
    pub access_key: String,
    pub secret_key: String,
    pub uuid: String,
    pub der_bzimage: String,
    pub der_init: String,
    pub der_initrd: String,
}

#[derive(Deserialize, Serialize)]
pub struct ExternalMeta {
    pub sha256: String,
    pub size: u64,
    pub format: String,
    pub internal: InternalMeta,
}

pub fn current_meta() -> Result<InternalMeta> {
    Ok(
        serde_json::from_slice(&read_bytes(Path::new("/organixm.json"))?)
            .map_err(|e| anyhow!("Failed to parse current system meta").context(e))?,
    )
}

pub fn retry<R, F: FnMut() -> Result<R>>(
    log: &Logger,
    total_time: Duration,
    period: Duration,
    mut f: F,
) -> Result<R> {
    let start = Utc::now();
    let mut count = 0;
    loop {
        count += 1;
        let e = match f() {
            Ok(r) => {
                return Ok(r);
            }
            Err(e) => e,
        };
        let now = Utc::now();
        let elapsed = now - start;
        if elapsed >= total_time && count >= 2 {
            return Err(
                anyhow!("Giving up after {} ({} attempts)", elapsed.hhmmss(), count).context(e),
            );
        }
        int_trace!(log, "Retry attempt failed", err = format!("{:?}", e));
        thread::sleep(period.to_std().unwrap());
    }
}

pub trait SimpleCommand {
    fn run(&mut self) -> Result<()>;
}

impl SimpleCommand for Command {
    fn run(&mut self) -> Result<()> {
        match match self.output() {
            Ok(o) => {
                if o.status.success() {
                    Ok(())
                } else {
                    Err(anyhow!("Exit code indicated error: {:?}", o))
                }
            }
            Err(e) => Err(e.into()),
        } {
            Ok(()) => Ok(()),
            Err(e) => Err(anyhow!("Failed to run {:?}", &self).context(e)),
        }
    }
}

pub fn file_digest(path: &Path, size: u64) -> Result<String> {
    let mut other_digest = sha2::Sha256::new();
    io::copy(&mut File::open(path)?.take(size), &mut other_digest)?;
    Ok(format!("{:x}", other_digest.finalize()))
}

pub fn version_bucket(version: &InternalMeta) -> Result<Bucket> {
    let mut bucket = Bucket::new(
        &version.bucket,
        s3::Region::from_str(&version.region)?,
        Credentials {
            access_key: Some(version.access_key.clone()),
            secret_key: Some(version.secret_key.clone()),
            security_token: None,
            session_token: None,
            expiration: None,
        },
    )?;
    bucket.set_request_timeout(Some(Duration::minutes(60).to_std().unwrap()));
    Ok(bucket)
}
