//! `plug reload`: ask the daemon to read its config again.

use crate::OutputFormat;
use crate::ui::{print_info_line, print_success_line};

pub(crate) async fn cmd_reload(output: &OutputFormat) -> anyhow::Result<()> {
    let req = plug_core::ipc::IpcRequest::Reload {
        auth_token: String::new(),
    };
    match crate::ipc_client::request(req).await? {
        plug_core::ipc::IpcResponse::Reloaded { report } => {
            if matches!(output, OutputFormat::Json) {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else if report.restart_required.is_empty() {
                print_success_line("Config reloaded.");
            } else {
                print_success_line("Config reloaded with restart-required changes.");
                for warning in report.restart_required {
                    print_info_line(format!("Restart required: {warning}"));
                }
                print_info_line(format!(
                    "{} before relying on those changes in live sessions.",
                    crate::runtime::restart_advice()
                ));
            }
        }
        plug_core::ipc::IpcResponse::Ok => {}
        plug_core::ipc::IpcResponse::Error { code, message } => {
            anyhow::bail!("{code}: {message}");
        }
        other => anyhow::bail!("unexpected daemon response: {other:?}"),
    }
    Ok(())
}
