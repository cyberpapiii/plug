//! Every AI client Plug can link, import from, and recognise, one row each.
//!
//! A row says what the client is called, where its settings files are, where
//! in them its MCP servers live and in what shape, and how to tell it is
//! installed. Linking, unlinking, repairing, importing, `plug clients` and
//! doctor all read the row, so adding a client is one variant and one row.

mod layout;
mod lazy;

use std::path::{Path, PathBuf};

use serde::Serialize;

pub use layout::{EntryShape, FileFormat, Layout, PlugLink, ServerEntry};
pub use lazy::lazy_tool_policy;

use crate::types::ClientType;

/// A client Plug knows how to link. Serialized as its variant name, which the
/// app reads in `plug import --output json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum ClientTarget {
    ClaudeDesktop,
    ClaudeCode,
    Cursor,
    Devin,
    VSCodeCopilot,
    CopilotCli,
    GeminiCli,
    CodexCli,
    GrokBuild,
    OpenCode,
    Zed,
    Cline,
    ClineCli,
    RooCode,
    Factory,
    Nanobot,
    Junie,
    Kilo,
    Pi,
    Warp,
    Kiro,
    KimiCode,
    QwenCode,
    Antigravity,
    Goose,
    Hermes,
    Amp,
    OpenClaw,
    LmStudio,
    MuseCode,
}

/// How a link reaches Plug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkTransport {
    /// `plug connect`, on this Mac.
    Stdio,
    /// Plug's HTTP endpoint.
    Http,
}

impl LinkTransport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
        }
    }
}

/// Which of a client's settings files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The one in the home folder.
    User,
    /// The one in the current folder.
    Project,
}

/// A settings file, relative to the home folder or to the current folder.
#[derive(Debug, Clone, Copy)]
pub enum Place {
    Home(&'static str),
    Here(&'static str),
}

/// A sign that a client is installed.
#[derive(Debug, Clone, Copy)]
pub enum Marker {
    /// An app bundle in `/Applications` or `~/Applications`.
    App(&'static str),
    Home(&'static str),
}

/// How to tell a client is installed.
#[derive(Debug, Clone, Copy)]
pub enum Detect {
    /// Its user settings file, or the folder it goes in, exists.
    SettingsFolder,
    /// One marker from each group exists. For clients whose settings folder
    /// exists whether or not they are installed.
    Markers(&'static [&'static [Marker]]),
}

/// One client.
#[derive(Debug)]
pub struct ClientInfo {
    pub target: ClientTarget,
    /// The one name the client goes by in config, `plug link`, and the
    /// `--client` argument a link carries.
    pub slug: &'static str,
    /// Other names `plug link` and `plug import` accept.
    pub aliases: &'static [&'static str],
    pub name: &'static str,
    /// What the client is taken for when it connects.
    pub live: Option<ClientType>,
    /// The `clientInfo.name` values it is known to send.
    pub reports: &'static [&'static str],
    /// For a version that sends another name: groups of words, lowercase,
    /// one of which the name holds all of. The longest match wins.
    pub reports_like: &'static [&'static [&'static str]],
    /// The file `plug link` writes, relative to the home folder.
    pub user_path: &'static str,
    /// The file `plug link --project` would write, relative to the current
    /// folder.
    pub project_path: Option<&'static str>,
    /// More files `plug import` reads.
    pub import_also: &'static [Place],
    pub layout: Layout,
    pub detect: Detect,
    /// Shown in `plug clients` and the link menus. A client that is not
    /// listed can still be linked and imported from by name.
    pub listed: bool,
}

const VSCODE_APP: Marker = Marker::App("Visual Studio Code.app");

const fn json(servers_at: &'static [&'static str], shape: EntryShape) -> Layout {
    Layout {
        format: FileFormat::Json,
        servers_at,
        also_reads: &[],
        shape,
        schema_version: None,
    }
}

const MCP_SERVERS: Layout = json(&["mcpServers"], EntryShape::Standard);
const TOML_MCP_SERVERS: Layout = Layout {
    format: FileFormat::Toml,
    servers_at: &["mcp_servers"],
    also_reads: &[],
    shape: EntryShape::Toml,
    schema_version: None,
};
const OPENCODE: Layout = json(&["mcp"], EntryShape::OpenCode);

const fn row(target: ClientTarget, slug: &'static str, name: &'static str) -> ClientInfo {
    ClientInfo {
        target,
        slug,
        aliases: &[],
        name,
        live: None,
        reports: &[],
        reports_like: &[],
        user_path: "",
        project_path: None,
        import_also: &[],
        layout: MCP_SERVERS,
        detect: Detect::SettingsFolder,
        listed: true,
    }
}

/// In `ClientTarget` order; `ClientTarget::info` indexes it.
static REGISTRY: [ClientInfo; 30] = [
    ClientInfo {
        live: Some(ClientType::ClaudeDesktop),
        reports: &["claude-ai"],
        reports_like: &[&["claude"]],
        user_path: "Library/Application Support/Claude/claude_desktop_config.json",
        ..row(
            ClientTarget::ClaudeDesktop,
            "claude-desktop",
            "Claude Desktop",
        )
    },
    ClientInfo {
        live: Some(ClientType::ClaudeCode),
        reports: &["claude-code"],
        reports_like: &[&["claude-code"], &["claude code"]],
        user_path: ".claude.json",
        project_path: Some(".mcp.json"),
        layout: json(&["mcpServers"], EntryShape::ClaudeCode),
        ..row(ClientTarget::ClaudeCode, "claude-code", "Claude Code")
    },
    ClientInfo {
        live: Some(ClientType::Cursor),
        reports: &["cursor-vscode"],
        reports_like: &[&["cursor"]],
        user_path: ".cursor/mcp.json",
        project_path: Some(".cursor/mcp.json"),
        ..row(ClientTarget::Cursor, "cursor", "Cursor")
    },
    // Devin's own file since Devin CLI v3000.3; Devin Desktop's default agent
    // reads it too. `windsurf` was the target name until Cognition renamed
    // the product. The Cascade file the Windsurf target wrote is import-only.
    // https://docs.devin.ai/cli/extensibility/mcp/configuration
    ClientInfo {
        aliases: &["windsurf"],
        live: Some(ClientType::Devin),
        reports: &["windsurf-client"],
        reports_like: &[&["devin"], &["cascade"], &["windsurf"], &["codeium"]],
        user_path: ".config/devin/mcp_config.json",
        project_path: Some(".devin/mcp_config.json"),
        import_also: &[Place::Home(".codeium/windsurf/mcp_config.json")],
        ..row(ClientTarget::Devin, "devin", "Devin")
    },
    // VS Code's own files keep servers under a top-level `servers`, in the
    // workspace file and the user profile file alike; older `settings.json`
    // nested them under `mcp`. Until 0.8.14 this target wrote
    // ~/.copilot/mcp-config.json, which belongs to Copilot CLI.
    // https://code.visualstudio.com/docs/copilot/customization/mcp-servers
    ClientInfo {
        live: Some(ClientType::VSCodeCopilot),
        reports: &["Visual-Studio-Code"],
        reports_like: &[&["copilot"], &["vscode"]],
        user_path: "Library/Application Support/Code/User/mcp.json",
        project_path: Some(".vscode/mcp.json"),
        import_also: &[Place::Home(".vscode/mcp.json")],
        layout: Layout {
            also_reads: &[&["mcp", "servers"]],
            ..json(&["servers"], EntryShape::VsCode)
        },
        detect: Detect::Markers(&[&[VSCODE_APP]]),
        ..row(ClientTarget::VSCodeCopilot, "vscode", "VS Code Copilot")
    },
    // The project files Copilot CLI reads, `.mcp.json` and `.github/mcp.json`,
    // are shared with clients that reject its extra fields, so it is linked
    // in its user file only. Its `clientInfo.name` is not documented; any
    // other Copilot is taken for VS Code, which keeps its tool ceiling.
    // https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers
    ClientInfo {
        aliases: &["copilot"],
        live: Some(ClientType::CopilotCli),
        reports: &[],
        reports_like: &[&["copilot", "cli"]],
        user_path: ".copilot/mcp-config.json",
        import_also: &[Place::Here(".github/mcp.json")],
        layout: json(&["mcpServers"], EntryShape::CopilotCli),
        ..row(
            ClientTarget::CopilotCli,
            "copilot-cli",
            "GitHub Copilot CLI",
        )
    },
    ClientInfo {
        aliases: &["gemini"],
        live: Some(ClientType::GeminiCli),
        reports: &["gemini-cli-mcp-client"],
        reports_like: &[&["gemini"]],
        user_path: ".gemini/settings.json",
        project_path: Some(".gemini/settings.json"),
        ..row(ClientTarget::GeminiCli, "gemini-cli", "Gemini CLI")
    },
    ClientInfo {
        aliases: &["codex"],
        live: Some(ClientType::CodexCli),
        reports: &[],
        reports_like: &[&["codex"]],
        user_path: ".codex/config.toml",
        import_also: &[Place::Here(".codex/config.toml")],
        layout: TOML_MCP_SERVERS,
        ..row(ClientTarget::CodexCli, "codex-cli", "Codex CLI")
    },
    // https://docs.x.ai/build/settings: the project file wins over the user
    // file for `[mcp_servers]`.
    ClientInfo {
        aliases: &["grok"],
        live: Some(ClientType::GrokBuild),
        reports: &[],
        reports_like: &[&["grok"]],
        user_path: ".grok/config.toml",
        project_path: Some(".grok/config.toml"),
        layout: TOML_MCP_SERVERS,
        ..row(ClientTarget::GrokBuild, "grok-build", "Grok Build")
    },
    ClientInfo {
        live: Some(ClientType::OpenCode),
        reports: &["opencode"],
        reports_like: &[&["opencode"]],
        user_path: ".config/opencode/opencode.json",
        project_path: Some("opencode.json"),
        layout: OPENCODE,
        ..row(ClientTarget::OpenCode, "opencode", "OpenCode")
    },
    ClientInfo {
        live: Some(ClientType::Zed),
        reports: &["Zed"],
        reports_like: &[&["zed"]],
        user_path: ".config/zed/settings.json",
        layout: json(&["context_servers"], EntryShape::Standard),
        ..row(ClientTarget::Zed, "zed", "Zed")
    },
    // `~/.vscode` exists for anyone who ever ran VS Code, so the app and the
    // extension's own folders say whether Cline is there.
    ClientInfo {
        user_path: ".vscode/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json",
        detect: Detect::Markers(&[
            &[VSCODE_APP],
            &[
                Marker::Home(
                    "Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev",
                ),
                Marker::Home(".vscode/extensions/saoudrizwan.claude-dev"),
            ],
        ]),
        ..row(ClientTarget::Cline, "cline", "Cline (VS Code)")
    },
    ClientInfo {
        user_path: ".cline/data/settings/cline_mcp_settings.json",
        detect: Detect::Markers(&[&[Marker::Home(".cline")]]),
        ..row(ClientTarget::ClineCli, "cline-cli", "Cline CLI")
    },
    // Left out of `plug clients` since 0.8.11.
    ClientInfo {
        user_path: ".roo/mcp.json",
        project_path: Some(".roo/mcp.json"),
        import_also: &[Place::Home(
            ".vscode/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json",
        )],
        listed: false,
        ..row(ClientTarget::RooCode, "roocode", "RooCode")
    },
    ClientInfo {
        user_path: ".factory/config.json",
        ..row(ClientTarget::Factory, "factory", "Factory")
    },
    ClientInfo {
        user_path: ".nanobot/config.json",
        project_path: Some(".nanobot/config.json"),
        layout: json(&["tools", "mcpServers"], EntryShape::Standard),
        ..row(ClientTarget::Nanobot, "nanobot", "Nanobot")
    },
    ClientInfo {
        user_path: ".junie/mcp/mcp.json",
        project_path: Some(".junie/mcp/mcp.json"),
        ..row(ClientTarget::Junie, "junie", "JetBrains Junie")
    },
    ClientInfo {
        user_path: ".config/kilo/opencode.json",
        project_path: Some("opencode.json"),
        layout: OPENCODE,
        ..row(ClientTarget::Kilo, "kilo", "Kilo Code")
    },
    // Built in since Pi 0.99.0.
    // https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/mcp.md
    ClientInfo {
        user_path: ".pi/agent/mcp.json",
        project_path: Some(".pi/mcp.json"),
        ..row(ClientTarget::Pi, "pi", "Pi")
    },
    // Warp starts the servers in these files on its own.
    // https://docs.warp.dev/agent-platform/capabilities/mcp/
    ClientInfo {
        user_path: ".warp/.mcp.json",
        project_path: Some(".warp/.mcp.json"),
        ..row(ClientTarget::Warp, "warp", "Warp")
    },
    // https://kiro.dev/docs/mcp/configuration/
    ClientInfo {
        user_path: ".kiro/settings/mcp.json",
        project_path: Some(".kiro/settings/mcp.json"),
        ..row(ClientTarget::Kiro, "kiro", "Kiro")
    },
    // https://moonshotai.github.io/kimi-code/en/customization/mcp.html
    ClientInfo {
        aliases: &["kimi"],
        user_path: ".kimi-code/mcp.json",
        project_path: Some(".kimi-code/mcp.json"),
        ..row(ClientTarget::KimiCode, "kimi-code", "Kimi Code")
    },
    // https://qwenlm.github.io/qwen-code-docs/en/users/features/mcp/
    ClientInfo {
        aliases: &["qwen"],
        user_path: ".qwen/settings.json",
        project_path: Some(".qwen/settings.json"),
        layout: json(&["mcpServers"], EntryShape::QwenCode),
        ..row(ClientTarget::QwenCode, "qwen-code", "Qwen Code")
    },
    ClientInfo {
        user_path: "Library/Application Support/Antigravity/antigravity_config.json",
        import_also: &[Place::Home(
            "Library/Application Support/Google/Antigravity/config.json",
        )],
        ..row(
            ClientTarget::Antigravity,
            "antigravity",
            "Google Antigravity",
        )
    },
    ClientInfo {
        user_path: ".config/goose/config.yaml",
        layout: Layout {
            format: FileFormat::Yaml,
            servers_at: &["extensions"],
            also_reads: &[],
            shape: EntryShape::Goose,
            schema_version: None,
        },
        ..row(ClientTarget::Goose, "goose", "Goose")
    },
    // One config file and no per-project one.
    // https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp
    ClientInfo {
        aliases: &["hermes-agent"],
        user_path: ".hermes/config.yaml",
        layout: Layout {
            format: FileFormat::Yaml,
            servers_at: &["mcp_servers"],
            also_reads: &[],
            shape: EntryShape::Standard,
            schema_version: None,
        },
        ..row(ClientTarget::Hermes, "hermes", "Hermes Agent")
    },
    // The four below were read off each maker's documentation on 2026-10-05
    // and have not been tried against an installed copy. Each has one file
    // Plug writes to and no per-project one.
    // https://ampcode.com/docs/cli/settings
    ClientInfo {
        user_path: ".config/amp/settings.json",
        layout: json(&["amp.mcpServers"], EntryShape::Standard),
        ..row(ClientTarget::Amp, "amp", "Amp")
    },
    // https://docs.openclaw.ai/gateway/configuration
    ClientInfo {
        user_path: ".openclaw/openclaw.json",
        layout: json(&["mcp", "servers"], EntryShape::OpenClaw),
        ..row(ClientTarget::OpenClaw, "openclaw", "OpenClaw")
    },
    // https://lmstudio.ai/docs/app/mcp
    ClientInfo {
        aliases: &["lmstudio"],
        user_path: ".lmstudio/mcp.json",
        ..row(ClientTarget::LmStudio, "lm-studio", "LM Studio")
    },
    // `muse` alone is Meta's agent, a remote client with no file to write.
    // Muse Code reads no settings file without `schema_version`.
    // https://dev.meta.ai/docs/muse-code/configuration
    ClientInfo {
        user_path: ".config/muse/settings.json",
        layout: Layout {
            schema_version: Some(1),
            ..json(&["mcp_servers"], EntryShape::MuseCode)
        },
        ..row(ClientTarget::MuseCode, "muse-code", "Muse Code")
    },
];

impl ClientTarget {
    pub fn info(self) -> &'static ClientInfo {
        &REGISTRY[self as usize]
    }

    pub fn all() -> impl Iterator<Item = ClientTarget> {
        REGISTRY.iter().map(|info| info.target)
    }

    /// The clients `plug clients` and the link menus show.
    pub fn listed() -> impl Iterator<Item = ClientTarget> {
        REGISTRY
            .iter()
            .filter(|info| info.listed)
            .map(|info| info.target)
    }

    /// The client a slug or alias names.
    pub fn from_slug(name: &str) -> Option<ClientTarget> {
        REGISTRY
            .iter()
            .find(|info| info.slug == name || info.aliases.contains(&name))
            .map(|info| info.target)
    }

    pub fn slug(self) -> &'static str {
        self.info().slug
    }

    pub fn display_name(self) -> &'static str {
        self.info().name
    }

    pub fn layout(self) -> &'static Layout {
        &self.info().layout
    }

    /// The arguments a stdio link runs Plug with. `--client` names the
    /// target the link was written for, so the daemon knows which client a
    /// connector serves without trusting the name the client reports.
    pub fn connect_args(self) -> [&'static str; 3] {
        ["connect", "--client", self.slug()]
    }

    /// Plug's entry for this client: `command` running `plug connect`, or
    /// `url`.
    pub fn plug_entry(
        self,
        transport: LinkTransport,
        command: &str,
        url: &str,
    ) -> serde_json::Value {
        let args = self.connect_args();
        self.layout().entry(&match transport {
            LinkTransport::Stdio => PlugLink::Stdio {
                command,
                args: &args,
            },
            LinkTransport::Http => PlugLink::Http { url },
        })
    }

    pub fn config_path(self, scope: Scope) -> Option<PathBuf> {
        let info = self.info();
        match scope {
            Scope::User => Some(dirs::home_dir()?.join(info.user_path)),
            Scope::Project => info.project_path.map(PathBuf::from),
        }
    }

    /// Every file `plug import` reads for this client.
    pub fn import_paths(self) -> Vec<PathBuf> {
        let Some(home) = dirs::home_dir() else {
            return Vec::new();
        };
        let info = self.info();
        let mut paths = vec![home.join(info.user_path)];
        paths.extend(info.project_path.map(PathBuf::from));
        paths.extend(info.import_also.iter().map(|place| match place {
            Place::Home(path) => home.join(path),
            Place::Here(path) => PathBuf::from(path),
        }));
        paths
    }

    pub fn is_installed(self) -> bool {
        let Some(home) = dirs::home_dir() else {
            return false;
        };
        match self.info().detect {
            Detect::SettingsFolder => {
                let path = home.join(self.info().user_path);
                // `~/.config` and the home folder are there for everyone.
                path.exists()
                    || path.parent().is_some_and(|parent| {
                        parent.exists() && !parent.ends_with(".config") && parent != home
                    })
            }
            Detect::Markers(groups) => groups
                .iter()
                .all(|group| group.iter().any(|marker| marker_exists(&home, marker))),
        }
    }
}

/// Whether `args` is how a link runs the connector: `connect`, alone as
/// older links wrote it or followed by `--client <target>`.
pub fn is_connect_args<S: AsRef<str>>(args: &[S]) -> bool {
    match args {
        [connect] => connect.as_ref() == "connect",
        [connect, flag, _target] => connect.as_ref() == "connect" && flag.as_ref() == "--client",
        _ => false,
    }
}

fn marker_exists(home: &Path, marker: &Marker) -> bool {
    match marker {
        Marker::App(app) => {
            Path::new("/Applications").join(app).exists()
                || home.join("Applications").join(app).exists()
        }
        Marker::Home(path) => home.join(path).exists(),
    }
}

impl std::str::FromStr for ClientTarget {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::from_slug(name).ok_or_else(|| format!("unknown client: {name}"))
    }
}

impl std::fmt::Display for ClientTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

impl ClientType {
    /// The link target for a client Plug recognised.
    pub fn target(&self) -> Option<ClientTarget> {
        REGISTRY
            .iter()
            .find(|info| info.live == Some(*self))
            .map(|info| info.target)
    }

    pub fn target_slug(&self) -> Option<&'static str> {
        self.target().map(ClientTarget::slug)
    }
}

impl std::fmt::Display for ClientType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match (self, self.target()) {
            (_, Some(target)) => target.display_name(),
            // Grok Bot reaches Plug over the public internet only, so it has
            // no file on this Mac to link.
            (ClientType::GrokBot, None) => "Grok Bot",
            (_, None) => "Unknown",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_sits_at_its_targets_index() {
        for (index, info) in REGISTRY.iter().enumerate() {
            assert_eq!(info.target as usize, index, "{} is out of order", info.slug);
            assert!(!info.user_path.is_empty(), "{} has no file", info.slug);
        }
    }

    #[test]
    fn every_name_a_client_goes_by_finds_it_and_no_other() {
        let mut seen = std::collections::HashSet::new();
        for target in ClientTarget::all() {
            let info = target.info();
            for name in std::iter::once(&info.slug).chain(info.aliases) {
                assert!(seen.insert(*name), "{name} names two clients");
                assert_eq!(name.parse::<ClientTarget>(), Ok(target));
            }
        }
        assert!("muse".parse::<ClientTarget>().is_err());
        assert_eq!(ClientTarget::Devin.to_string(), "Devin");
    }

    #[test]
    fn a_recognised_client_maps_to_its_target_and_back() {
        for target in ClientTarget::all() {
            if let Some(live) = target.info().live {
                assert_eq!(live.target(), Some(target));
                assert_eq!(live.to_string(), target.display_name());
            }
        }
        assert_eq!(ClientType::VSCodeCopilot.target_slug(), Some("vscode"));
        assert_eq!(ClientType::GrokBot.target_slug(), None);
        assert_eq!(ClientType::GrokBot.to_string(), "Grok Bot");
        assert_eq!(ClientType::Unknown.to_string(), "Unknown");
    }

    #[test]
    fn only_roocode_is_left_out_of_the_list() {
        let unlisted: Vec<_> = ClientTarget::all()
            .filter(|target| !target.info().listed)
            .collect();
        assert_eq!(unlisted, [ClientTarget::RooCode]);
    }

    #[test]
    fn connect_args_are_recognised_with_and_without_a_target() {
        assert!(is_connect_args(&ClientTarget::Cursor.connect_args()));
        assert!(is_connect_args(&["connect"]));
        assert!(!is_connect_args(&["serve"]));
        assert!(!is_connect_args(&["connect", "--client"]));
        assert!(!is_connect_args(&["connect", "--config", "/tmp/x.toml"]));
        assert!(!is_connect_args::<&str>(&[]));
    }

    #[test]
    fn vscode_and_copilot_cli_do_not_share_a_user_file() {
        assert_ne!(
            ClientTarget::VSCodeCopilot.config_path(Scope::User),
            ClientTarget::CopilotCli.config_path(Scope::User)
        );
        assert_eq!(ClientTarget::CopilotCli.config_path(Scope::Project), None);
        assert_eq!(
            ClientTarget::Pi.config_path(Scope::Project),
            Some(PathBuf::from(".pi/mcp.json"))
        );
    }
}
