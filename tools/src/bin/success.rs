use std::process::{exit, Command};

use anyhow::Result;
use slog::Logger;
use sloggers::{
    terminal::{Destination, TerminalLoggerBuilder},
    types::Severity,
    Build,
};
use tools::{current_meta, mount_boot, SimpleCommand};
use tools::{int_err, int_info};

fn main_inner(log: Logger) -> Result<()> {
    let current = current_meta()?;
    let _mount = mount_boot(log.clone())?;
    Command::new("grub-set-default").arg(current.uuid).run()?;
    Ok(())
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
