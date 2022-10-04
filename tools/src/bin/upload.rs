use std::{fs::File, path::PathBuf, process::exit, str::FromStr};

use anyhow::Result;
use clap::Parser;
use s3::{creds::Credentials, Bucket};
use sloggers::{
    terminal::{Destination, TerminalLoggerBuilder},
    types::Severity,
    Build,
};
use tools::{int_err, int_info};
use tools::{read_bytes, ExternalMeta};

#[derive(Parser, Debug)]
#[clap()]
struct Args {
    #[clap()]
    version_meta: PathBuf,

    #[clap()]
    image: PathBuf,
}

fn main_inner() -> Result<()> {
    let args = Args::parse();
    let version: ExternalMeta =
        // Can't meaningfully wrap this either due to rust or serde design decisions...
         serde_json::from_slice(&read_bytes(&args.version_meta)?)?;
    let bucket = Bucket::new(
        &version.internal.bucket,
        s3::Region::from_str(&version.internal.region)?,
        Credentials::from_env()?,
    )?;

    bucket.put_object_stream(&mut File::open(&args.image)?, &version.internal.object_path)?;
    bucket.put_object(
        &format!("{}.meta", version.internal.object_path),
        &serde_json::to_vec(&version).unwrap(),
    )?;

    Ok(())
}

fn main() {
    fn main0() -> bool {
        let mut builder = TerminalLoggerBuilder::new();
        builder.level(Severity::Debug);
        builder.destination(Destination::Stderr);
        let root_log = builder.build().unwrap();
        match main_inner() {
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
