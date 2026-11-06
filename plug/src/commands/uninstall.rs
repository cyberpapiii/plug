//! `plug uninstall-cleanup`: remove what the app put outside its bundle.

use crate::OutputFormat;
use crate::ui::{print_info_line, print_success_line};

pub(crate) fn cmd_uninstall_cleanup(output: &OutputFormat) -> anyhow::Result<()> {
    let report = crate::install::uninstall_cleanup()?;
    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => {
            for item in report.items {
                if item.changed {
                    print_success_line(item.message);
                } else {
                    print_info_line(item.message);
                }
            }
        }
    }
    Ok(())
}
