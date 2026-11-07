//! Where a client's settings file keeps its MCP servers, and in what shape.
//!
//! One description serves every direction: writing Plug's entry, finding it
//! again, taking it out, pointing it at a new command, and reading the other
//! servers for import. JSON is parsed and written back whole; TOML and YAML
//! are edited as text, so the person's comments and key order survive.

use serde_json::{Map, Value, json};

use crate::config::{HttpConfig, TransportType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    Json,
    Toml,
    Yaml,
}

/// The fields of one server entry, as each client spells them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryShape {
    /// `command` and `args`, or `url`.
    Standard,
    /// An HTTP entry names its type; without it Claude Code reads the entry
    /// as a command with none given.
    ClaudeCode,
    /// Every entry names its type.
    VsCode,
    /// `type` and `tools` are required on every entry; `"*"` allows every
    /// tool.
    CopilotCli,
    /// Qwen Code reads `url` as the older SSE transport, so an HTTP server's
    /// address is `httpUrl`.
    QwenCode,
    /// Each entry `local` with its command and arguments in one list, or
    /// `remote` with a URL. Any other top-level key stops OpenCode starting.
    OpenCode,
    /// OpenClaw refuses to start on a key it does not know, so an entry
    /// carries only what its documentation names.
    OpenClaw,
    /// Every entry names its transport. `optional` lets Muse Code start when
    /// Plug is not running; its default stops the whole run.
    MuseCode,
    /// `type: stdio` or `type: sse` with a `uri`.
    Goose,
    /// `[mcp_servers.<name>]` tables; HTTP says `transport = "http"`.
    Toml,
}

#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub format: FileFormat,
    /// The keys, outermost first, of the table that holds the servers.
    pub servers_at: &'static [&'static str],
    /// Older places the servers were kept, still read.
    pub also_reads: &'static [&'static [&'static str]],
    pub shape: EntryShape,
    /// A `schema_version` the client needs at the top of the file.
    pub schema_version: Option<u64>,
}

/// What Plug's own entry runs.
#[derive(Debug, Clone, Copy)]
pub enum PlugLink<'a> {
    Stdio {
        command: &'a str,
        args: &'a [&'a str],
    },
    Http {
        url: &'a str,
    },
}

/// One server entry as Plug reads it, whatever the client's shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEntry {
    pub transport: TransportType,
    pub command: Option<String>,
    /// `None` when the entry's arguments are not all strings.
    pub args: Option<Vec<String>>,
    pub url: Option<String>,
    pub env: Vec<(String, String)>,
    pub enabled: bool,
}

/// Plug's entry is called `plug`. Links written by hand are often `Plug`.
fn is_plug(name: &str) -> bool {
    name.eq_ignore_ascii_case("plug")
}

/// Whether an entry is Plug itself, which importing would point Plug at:
/// one named as Plug's entry is, or one under any name that runs
/// `plug connect` or `plug serve`, or names Plug's own address. The app's
/// paste reader holds to the same rule but for the address, which it does
/// not know; `testdata/server_entries.json` pins both.
fn is_plug_itself(name: &str, entry: &ServerEntry, plug: &HttpConfig) -> bool {
    let runs_plug = entry.command.as_deref().is_some_and(|command| {
        std::path::Path::new(command).file_name() == Some("plug".as_ref())
            && entry
                .args
                .iter()
                .flatten()
                .any(|arg| arg == "connect" || arg == "serve")
    });
    let reaches_plug = entry
        .url
        .as_deref()
        .is_some_and(|url| plug.is_own_mcp_url(url));
    is_plug(name) || runs_plug || reaches_plug
}

impl Layout {
    /// Plug's entry, in this client's shape.
    pub fn entry(&self, link: &PlugLink) -> Value {
        match (self.shape, *link) {
            (EntryShape::OpenCode, PlugLink::Stdio { command, args }) => {
                let mut words = vec![command];
                words.extend_from_slice(args);
                json!({ "type": "local", "command": words, "enabled": true })
            }
            (EntryShape::OpenCode, PlugLink::Http { url }) => {
                json!({ "type": "remote", "url": url, "enabled": true })
            }
            (EntryShape::VsCode, PlugLink::Stdio { command, args }) => {
                json!({ "type": "stdio", "command": command, "args": args })
            }
            (EntryShape::CopilotCli, PlugLink::Stdio { command, args }) => {
                json!({ "type": "local", "command": command, "args": args, "tools": ["*"] })
            }
            (EntryShape::CopilotCli, PlugLink::Http { url }) => {
                json!({ "type": "http", "url": url, "tools": ["*"] })
            }
            (EntryShape::MuseCode, PlugLink::Stdio { command, args }) => json!({
                "transport": "stdio", "command": command, "args": args, "mode": "optional"
            }),
            (EntryShape::MuseCode, PlugLink::Http { url }) => json!({
                "transport": "streamable_http", "url": url, "mode": "optional"
            }),
            (EntryShape::Goose, PlugLink::Stdio { command, args }) => {
                json!({ "type": "stdio", "command": command, "args": args, "enabled": true })
            }
            (EntryShape::Goose, PlugLink::Http { url }) => {
                json!({ "type": "sse", "uri": url, "enabled": true })
            }
            (EntryShape::ClaudeCode | EntryShape::VsCode, PlugLink::Http { url }) => {
                json!({ "type": "http", "url": url })
            }
            (EntryShape::QwenCode, PlugLink::Http { url }) => json!({ "httpUrl": url }),
            (EntryShape::OpenClaw, PlugLink::Http { url }) => {
                json!({ "url": url, "transport": "streamable-http" })
            }
            (EntryShape::Toml, PlugLink::Http { url }) => {
                json!({ "transport": "http", "url": url })
            }
            (_, PlugLink::Stdio { command, args }) => json!({ "command": command, "args": args }),
            (_, PlugLink::Http { url }) => json!({ "url": url }),
        }
    }

    /// A settings file holding only Plug's entry.
    pub fn snippet(&self, entry: &Value) -> String {
        let mut document = json!({ "plug": entry });
        for key in self.servers_at.iter().rev() {
            document = Value::Object(Map::from_iter([(key.to_string(), document)]));
        }
        match self.format {
            FileFormat::Json => pretty_json(&document),
            FileFormat::Toml => toml::to_string(&document).expect("a JSON object is a TOML table"),
            FileFormat::Yaml => serde_norway::to_string(&document).expect("a JSON value is YAML"),
        }
    }

    /// `existing` with Plug's entry put in, replacing any it had.
    pub fn link(&self, existing: &str, entry: &Value) -> anyhow::Result<String> {
        match self.format {
            FileFormat::Json => {
                let mut document = read_json(existing)?;
                let mut servers = &mut document;
                for key in self.servers_at {
                    let object = servers.as_object_mut().ok_or_else(|| {
                        anyhow::anyhow!("`{key}` sits inside something that is not a table")
                    })?;
                    let slot = object.entry(*key).or_insert_with(|| json!({}));
                    if !slot.is_object() {
                        *slot = json!({});
                    }
                    servers = slot;
                }
                let servers = servers.as_object_mut().expect("made a table above");
                servers.retain(|name, _| !is_plug(name));
                servers.insert("plug".to_string(), entry.clone());
                if let (Some(version), Some(settings)) =
                    (self.schema_version, document.as_object_mut())
                {
                    settings
                        .entry("schema_version")
                        .or_insert_with(|| json!(version));
                }
                Ok(pretty_json(&document))
            }
            FileFormat::Toml => {
                let mut text = self.unlink_toml(existing);
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&self.snippet(entry));
                Ok(text)
            }
            FileFormat::Yaml => self.link_yaml(existing, entry),
        }
    }

    /// `existing` without Plug's entry, and otherwise as it was.
    pub fn unlink(&self, existing: &str) -> anyhow::Result<String> {
        match self.format {
            FileFormat::Json => {
                let mut document = read_json(existing)?;
                for path in self.paths() {
                    if let Some(servers) = navigate_mut(&mut document, path) {
                        servers.retain(|name, _| !is_plug(name));
                    }
                }
                Ok(pretty_json(&document))
            }
            FileFormat::Toml => Ok(self.unlink_toml(existing)),
            FileFormat::Yaml => Ok(unlink_yaml(existing, self.yaml_key())),
        }
    }

    /// Plug's entry, if the file has one.
    pub fn read_plug(&self, content: &str) -> Option<ServerEntry> {
        let servers = self.servers(content).ok()??;
        servers
            .iter()
            .find(|(name, _)| is_plug(name))
            .and_then(|(_, entry)| read_entry(entry))
    }

    /// Every server in the file but Plug itself, which serves at `plug`.
    pub fn read_servers(
        &self,
        content: &str,
        plug: &HttpConfig,
    ) -> Result<Vec<(String, ServerEntry)>, String> {
        Ok(self
            .entries(content)?
            .into_iter()
            .filter(|(name, entry)| !is_plug_itself(name, entry, plug))
            .collect())
    }

    /// Every readable entry in the file but those named as Plug's own is.
    fn entries(&self, content: &str) -> Result<Vec<(String, ServerEntry)>, String> {
        // A file Plug was linked into twice has two `[mcp_servers.plug]`
        // tables, which no TOML parser accepts.
        let content = match self.format {
            FileFormat::Toml => self.unlink_toml(content),
            _ => content.to_string(),
        };
        Ok(self
            .servers(&content)?
            .unwrap_or_default()
            .into_iter()
            .filter(|(name, _)| !is_plug(name))
            .filter_map(|(name, entry)| Some((name, read_entry(&entry)?)))
            .collect())
    }

    /// How many entries for Plug, which serves at `plug`, the file has.
    /// More than one is a mistake to clean up.
    pub fn plug_entry_count(&self, content: &str, plug: &HttpConfig) -> usize {
        let renamed = self.entries(content).map_or(0, |entries| {
            entries
                .iter()
                .filter(|(name, entry)| is_plug_itself(name, entry, plug))
                .count()
        });
        renamed + self.named_plug_entry_count(content)
    }

    fn named_plug_entry_count(&self, content: &str) -> usize {
        match self.format {
            FileFormat::Json => self.servers(content).ok().flatten().map_or(0, |servers| {
                servers.keys().filter(|name| is_plug(name)).count()
            }),
            FileFormat::Toml => content
                .lines()
                .filter(|line| self.toml_plug_header(line.trim()) == Some(TomlHeader::Entry))
                .count(),
            FileFormat::Yaml => {
                let lines: Vec<String> = content.lines().map(str::to_string).collect();
                yaml_plug_lines(&lines, self.yaml_key()).len()
            }
        }
    }

    /// Whether the file parses at all.
    pub fn check_syntax(&self, content: &str) -> Result<(), String> {
        match self.format {
            FileFormat::Json => serde_json::from_str::<Value>(content)
                .map(drop)
                .map_err(|error| format!("invalid JSON: {error}")),
            FileFormat::Toml => toml::from_str::<toml::Table>(content)
                .map(drop)
                .map_err(|error| format!("invalid TOML: {error}")),
            FileFormat::Yaml => serde_norway::from_str::<serde_norway::Value>(content)
                .map(drop)
                .map_err(|error| format!("invalid YAML: {error}")),
        }
    }

    /// `content` with Plug's stdio entry running `command` with `args`, and
    /// every other field and line left as it was.
    pub fn set_plug_command(
        &self,
        content: &str,
        command: &str,
        args: &[&str],
    ) -> anyhow::Result<String> {
        match self.format {
            FileFormat::Json => {
                let mut document: Value = serde_json::from_str(content)?;
                let plug = self
                    .paths()
                    .find_map(|path| {
                        navigate(&document, path)?
                            .keys()
                            .find(|name| is_plug(name))
                            .map(|name| (path, name.clone()))
                    })
                    .and_then(|(path, name)| navigate_mut(&mut document, path)?.get_mut(&name))
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| anyhow::anyhow!("missing JSON Plug entry"))?;
                if self.shape == EntryShape::OpenCode {
                    let mut words = vec![command];
                    words.extend_from_slice(args);
                    plug.insert("command".to_string(), json!(words));
                } else {
                    plug.insert("command".to_string(), json!(command));
                    plug.insert("args".to_string(), json!(args));
                }
                Ok(pretty_json(&document))
            }
            FileFormat::Toml => {
                let mut lines = text_lines(content);
                let start = lines
                    .iter()
                    .position(|line| self.toml_plug_header(line.trim()) == Some(TomlHeader::Entry))
                    .ok_or_else(|| {
                        anyhow::anyhow!("missing Plug entry under {}", self.toml_key())
                    })?
                    + 1;
                let end = lines[start..]
                    .iter()
                    .position(|line| line.trim_start().starts_with('['))
                    .map_or(lines.len(), |offset| start + offset);
                let args =
                    toml::Value::Array(args.iter().map(|arg| toml::Value::from(*arg)).collect());
                replace_assignments(
                    &mut lines,
                    start,
                    end,
                    &[
                        ("command", toml::Value::from(command).to_string()),
                        ("args", args.to_string()),
                    ],
                    Syntax::Toml,
                )?;
                Ok(join_lines(content, lines))
            }
            FileFormat::Yaml => {
                let key = self.yaml_key();
                let mut lines = text_lines(content);
                let (plug, end) = yaml_plug_block(&lines, key)
                    .ok_or_else(|| anyhow::anyhow!("missing Plug entry under {key}"))?;
                let args = args.iter().map(|arg| yaml_scalar(arg)).collect::<Vec<_>>();
                replace_assignments(
                    &mut lines,
                    plug + 1,
                    end,
                    &[
                        ("command", yaml_scalar(command)),
                        ("args", format!("[{}]", args.join(", "))),
                    ],
                    Syntax::Yaml,
                )?;
                Ok(join_lines(content, lines))
            }
        }
    }

    fn paths(&self) -> impl Iterator<Item = &'static [&'static str]> {
        std::iter::once(self.servers_at).chain(self.also_reads.iter().copied())
    }

    /// The servers table, from the first place the file has one.
    fn servers(&self, content: &str) -> Result<Option<Map<String, Value>>, String> {
        let document = match self.format {
            FileFormat::Json => serde_json::from_str::<Value>(content)
                .map_err(|error| format!("JSON parse error: {error}"))?,
            FileFormat::Toml => serde_json::to_value(
                toml::from_str::<toml::Table>(content)
                    .map_err(|error| format!("TOML parse error: {error}"))?,
            )
            .map_err(|error| error.to_string())?,
            FileFormat::Yaml => {
                let document = serde_norway::from_str::<serde_norway::Value>(content)
                    .map_err(|error| format!("YAML parse error: {error}"))?;
                // Only the servers table is turned into JSON: elsewhere the
                // file may have keys that are not strings.
                let Some(servers) = document.get(self.yaml_key()).and_then(|v| v.as_mapping())
                else {
                    return Ok(None);
                };
                return Ok(Some(
                    servers
                        .iter()
                        .filter_map(|(name, entry)| {
                            Some((
                                name.as_str()?.to_string(),
                                serde_json::to_value(entry).ok()?,
                            ))
                        })
                        .collect(),
                ));
            }
        };
        Ok(self
            .paths()
            .find_map(|path| navigate(&document, path))
            .cloned())
    }

    fn yaml_key(&self) -> &'static str {
        self.servers_at[0]
    }

    fn toml_key(&self) -> String {
        self.servers_at.join(".")
    }

    fn toml_plug_header(&self, line: &str) -> Option<TomlHeader> {
        let name = line
            .strip_prefix("[[")
            .and_then(|rest| rest.strip_suffix("]]"))
            .or_else(|| {
                line.strip_prefix('[')
                    .and_then(|rest| rest.strip_suffix(']'))
            })?;
        let key = self.toml_key();
        let rest = name.trim().strip_prefix(key.as_str())?.strip_prefix('.')?;
        let rest = rest
            .strip_prefix("\"plug\"")
            .or_else(|| rest.strip_prefix("plug"))?;
        match rest {
            "" => Some(TomlHeader::Entry),
            _ if rest.starts_with('.') => Some(TomlHeader::Inside),
            _ => None,
        }
    }

    /// `content` without Plug's tables: its own and any below it, such as
    /// `[mcp_servers.plug.env]`.
    fn unlink_toml(&self, content: &str) -> String {
        let mut output = Vec::new();
        let mut skipping = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                skipping = self.toml_plug_header(trimmed).is_some();
            }
            if !skipping {
                output.push(line);
            }
        }
        join_lines(content, output.into_iter().map(str::to_string).collect())
    }

    /// Put Plug's entry into a YAML file as text, so comments, key order, and
    /// every other line stay exactly as they were.
    fn link_yaml(&self, existing: &str, entry: &Value) -> anyhow::Result<String> {
        let refused = |detail: &str| {
            anyhow::anyhow!(
                "the client's config file {detail}, so Plug left it alone; fix or move the file and try again"
            )
        };
        if !existing.trim().is_empty()
            && !serde_norway::from_str::<serde_norway::Value>(existing)
                .is_ok_and(|value| value.is_mapping())
        {
            return Err(refused("is not valid YAML"));
        }
        let key = self.yaml_key();
        let heading = format!("{key}:");
        let snippet = self.snippet(entry);
        let entry_lines = snippet.lines().skip(1).map(str::to_string);
        let mut lines = text_lines(&unlink_yaml(existing, key));
        match lines.iter().position(|line| line.trim_end() == heading) {
            Some(parent) => {
                // Match the indentation the file's other servers already use.
                let indent = lines[parent + 1..]
                    .iter()
                    .find(|line| !line.trim().is_empty())
                    .map(|line| indentation(line))
                    .filter(|indent| *indent > 0)
                    .unwrap_or(2);
                let pad = " ".repeat(indent.saturating_sub(2));
                let entry = entry_lines.map(|line| format!("{pad}{line}"));
                lines.splice(parent + 1..parent + 1, entry);
            }
            None if lines
                .iter()
                .any(|line| indentation(line) == 0 && line.starts_with(&heading)) =>
            {
                return Err(refused(&format!(
                    "writes `{heading}` on one line; add Plug to it by hand"
                )));
            }
            None => {
                lines.push(heading);
                lines.extend(entry_lines);
            }
        }
        let updated = lines.join("\n") + "\n";
        serde_norway::from_str::<serde_norway::Value>(&updated)
            .map_err(|error| refused(&format!("would not be valid YAML ({error})")))?;
        Ok(updated)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TomlHeader {
    /// `[mcp_servers.plug]` itself.
    Entry,
    /// A table below it.
    Inside,
}

fn navigate<'a>(document: &'a Value, path: &[&str]) -> Option<&'a Map<String, Value>> {
    path.iter()
        .try_fold(document, |value, key| value.get(*key))?
        .as_object()
}

fn navigate_mut<'a>(document: &'a mut Value, path: &[&str]) -> Option<&'a mut Map<String, Value>> {
    path.iter()
        .try_fold(document, |value, key| value.get_mut(*key))?
        .as_object_mut()
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("a JSON value serializes")
}

/// A client's JSON settings as a value. An empty file is an empty object.
///
/// A file that is not plain JSON, such as one with comments, is refused:
/// writing it back would drop everything Plug could not read.
fn read_json(existing: &str) -> anyhow::Result<Value> {
    if existing.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(existing).map_err(|error| {
        anyhow::anyhow!(
            "this client's settings file is not plain JSON ({error}), so Plug left it alone; \
             add Plug's entry to it by hand"
        )
    })
}

fn read_entry(entry: &Value) -> Option<ServerEntry> {
    let object = entry.as_object()?;
    let text = |key: &str| object.get(key).and_then(Value::as_str).map(str::to_owned);
    let strings = |value: &Value| -> Option<Vec<String>> {
        value
            .as_array()?
            .iter()
            .map(|word| word.as_str().map(str::to_owned))
            .collect()
    };

    let url = ["url", "httpUrl", "sseUrl", "uri", "http_url"]
        .into_iter()
        .find_map(text);
    // OpenCode keeps the command and its arguments in one list.
    let (command, mut args) = match object.get("command") {
        Some(words @ Value::Array(_)) => match strings(words) {
            Some(mut words) if !words.is_empty() => {
                let command = words.remove(0);
                (Some(command), Some(words))
            }
            _ => (None, None),
        },
        other => (
            other.and_then(Value::as_str).map(str::to_owned),
            Some(Vec::new()),
        ),
    };
    if let Some(listed) = object.get("args") {
        args = match (args, strings(listed)) {
            (Some(mut args), Some(listed)) => {
                args.extend(listed);
                Some(args)
            }
            _ => None,
        };
    }
    let kind = text("type").or_else(|| text("transport"));
    let transport = match (kind.as_deref(), &url, &command) {
        (Some("sse"), Some(_), _) => TransportType::Sse,
        (_, Some(_), _) => TransportType::Http,
        (_, None, Some(_)) => TransportType::Stdio,
        (_, None, None) => return None,
    };
    let env = object
        .get("env")
        .or_else(|| object.get("environment"))
        .and_then(Value::as_object)
        .map(|env| {
            env.iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    Some(ServerEntry {
        command: if url.is_some() { None } else { command },
        args,
        url,
        transport,
        env,
        enabled: object
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

fn yaml_scalar(value: &str) -> String {
    serde_norway::to_string(&Value::from(value))
        .expect("serializing a YAML string cannot fail")
        .trim()
        .to_string()
}

fn text_lines(content: &str) -> Vec<String> {
    content.lines().map(str::to_string).collect()
}

fn join_lines(original: &str, lines: Vec<String>) -> String {
    let mut result = lines.join("\n");
    if original.ends_with('\n') && !result.is_empty() {
        result.push('\n');
    }
    result
}

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The `plug:` lines directly under `key:`.
fn yaml_plug_lines(lines: &[String], key: &str) -> Vec<usize> {
    let heading = format!("{key}:");
    let Some(parent) = lines.iter().position(|line| line.trim_end() == heading) else {
        return Vec::new();
    };
    let section_end = lines[parent + 1..]
        .iter()
        .position(|line| !line.trim().is_empty() && indentation(line) == 0)
        .map_or(lines.len(), |offset| parent + 1 + offset);
    let Some(child_indent) = lines[parent + 1..section_end]
        .iter()
        .find(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .map(|line| indentation(line))
    else {
        return Vec::new();
    };
    (parent + 1..section_end)
        .filter(|&index| {
            indentation(&lines[index]) == child_indent
                && lines[index].trim().strip_suffix(':').is_some_and(is_plug)
        })
        .collect()
}

/// The lines of the Plug entry under `key`: the `plug:` line and one past
/// the entry's last line.
fn yaml_plug_block(lines: &[String], key: &str) -> Option<(usize, usize)> {
    let plug = *yaml_plug_lines(lines, key).first()?;
    let plug_indent = indentation(&lines[plug]);
    let end = (plug + 1..lines.len())
        .find(|&index| !lines[index].trim().is_empty() && indentation(&lines[index]) <= plug_indent)
        .unwrap_or(lines.len());
    Some((plug, end))
}

/// Remove the Plug entries under `key`, and nothing else.
fn unlink_yaml(existing: &str, key: &str) -> String {
    let mut lines = text_lines(existing);
    while let Some((plug, mut end)) = yaml_plug_block(&lines, key) {
        // Blank lines after the entry belong to whatever follows it.
        while end > plug + 1 && lines[end - 1].trim().is_empty() {
            end -= 1;
        }
        lines.drain(plug..end);
    }
    join_lines(existing, lines)
}

#[derive(Clone, Copy)]
enum Syntax {
    Toml,
    Yaml,
}

fn replace_assignments(
    lines: &mut Vec<String>,
    start: usize,
    end: usize,
    replacements: &[(&str, String)],
    syntax: Syntax,
) -> anyhow::Result<()> {
    let mut section_end = end;
    for (key, value) in replacements {
        let found = (start..section_end).find(|&index| {
            lines[index]
                .trim_start()
                .strip_prefix(key)
                .map(str::trim_start)
                .is_some_and(|rest| rest.starts_with('=') || rest.starts_with(':'))
        });
        let Some(index) = found else {
            anyhow::bail!("missing Plug {key} entry");
        };
        let line = &lines[index];
        let after_key = line.trim_start()[key.len()..].trim_start();
        let separator = if after_key.starts_with('=') { '=' } else { ':' };
        let separator_index = line.find(separator).expect("assignment separator");
        let value_end = match syntax {
            Syntax::Toml => toml_value_end(lines, index, section_end, separator_index),
            Syntax::Yaml => yaml_value_end(lines, index, section_end, separator_index),
        };
        // A value over several lines keeps the comment after its end.
        let comment = if value_end > index + 1 {
            inline_comment(&lines[value_end - 1])
        } else {
            inline_comment(&line[separator_index + 1..])
        }
        .to_string();
        lines[index] = format!("{} {value}{comment}", &line[..=separator_index]);
        if value_end > index + 1 {
            lines.drain(index + 1..value_end);
            section_end -= value_end - index - 1;
        }
    }
    Ok(())
}

fn toml_value_end(lines: &[String], start: usize, end: usize, separator_index: usize) -> usize {
    if !lines[start][separator_index + 1..]
        .trim_start()
        .starts_with('[')
    {
        return start + 1;
    }
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (line_index, line) in lines.iter().enumerate().take(end).skip(start) {
        let value = if line_index == start {
            &line[separator_index + 1..]
        } else {
            line.as_str()
        };
        for character in value.chars() {
            match character {
                '\\' if quoted => escaped = !escaped,
                '"' if !escaped => quoted = !quoted,
                '#' if !quoted => break,
                '[' if !quoted => depth += 1,
                ']' if !quoted && depth > 0 => depth -= 1,
                _ => escaped = false,
            }
        }
        if depth == 0 {
            return line_index + 1;
        }
    }
    start + 1
}

fn yaml_value_end(lines: &[String], start: usize, end: usize, separator_index: usize) -> usize {
    let value = &lines[start][separator_index + 1..];
    let value = value[..value.len() - inline_comment(value).len()].trim();
    if !value.is_empty() {
        return start + 1;
    }
    let assignment_indent = indentation(&lines[start]);
    let mut value_end = start + 1;
    while value_end < end
        && (lines[value_end].trim().is_empty()
            || indentation(&lines[value_end]) > assignment_indent)
    {
        value_end += 1;
    }
    value_end
}

fn inline_comment(value: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        match character {
            '\\' if quoted => escaped = !escaped,
            '"' if !escaped => quoted = !quoted,
            '#' if !quoted => {
                let comment_start = value[..index]
                    .char_indices()
                    .rev()
                    .find(|(_, character)| !character.is_whitespace())
                    .map_or(0, |(index, character)| index + character.len_utf8());
                return &value[comment_start..];
            }
            _ => escaped = false,
        }
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::{ClientTarget, LinkTransport};

    const URL: &str = "https://plug.example.com/mcp";

    /// Plug serving at [`URL`].
    fn plug_at_url() -> HttpConfig {
        HttpConfig {
            public_base_url: Some("https://plug.example.com".to_string()),
            ..HttpConfig::default()
        }
    }

    /// What every client's link is written as reads back as the same link,
    /// and the same entry under another name is what import finds.
    #[test]
    fn every_clients_link_and_servers_read_back_as_written() {
        for target in ClientTarget::all() {
            let layout = target.layout();
            for transport in [LinkTransport::Stdio, LinkTransport::Http] {
                let entry = target.plug_entry(transport, "/usr/local/bin/plug", URL);
                let written = layout.link("", &entry).unwrap();
                let plug = layout
                    .read_plug(&written)
                    .unwrap_or_else(|| panic!("{target:?} lost its {transport:?} link: {written}"));
                match transport {
                    LinkTransport::Stdio => {
                        assert_eq!(plug.transport, TransportType::Stdio, "{target:?}");
                        assert_eq!(plug.command.as_deref(), Some("/usr/local/bin/plug"));
                        assert_eq!(
                            plug.args.as_deref(),
                            Some(&target.connect_args().map(String::from)[..]),
                            "{target:?}"
                        );
                    }
                    LinkTransport::Http => {
                        assert_eq!(plug.url.as_deref(), Some(URL), "{target:?}");
                        assert_ne!(plug.transport, TransportType::Stdio, "{target:?}");
                    }
                }
                let own = plug_at_url();
                assert_eq!(layout.plug_entry_count(&written, &own), 1, "{target:?}");
                assert!(layout.read_servers(&written, &own).unwrap().is_empty());

                let renamed = written
                    .replace("plug:", "github:")
                    .replace("\"plug\"", "\"github\"")
                    .replace(".plug]", ".github]");
                // Under another name, a link that runs Plug or names its
                // address is still Plug.
                assert!(
                    layout.read_servers(&renamed, &own).unwrap().is_empty(),
                    "{target:?}: {renamed}"
                );
                assert_eq!(layout.plug_entry_count(&renamed, &own), 1, "{target:?}");
                let renamed = renamed
                    .replace("/usr/local/bin/plug", "/usr/local/bin/github-mcp")
                    .replace(URL, "https://github.example.com/mcp");
                let imported = layout.read_servers(&renamed, &own).unwrap();
                assert_eq!(
                    imported.len(),
                    1,
                    "{target:?} import found nothing: {renamed}"
                );
                assert_eq!(imported[0].0, "github");
                let expected = ServerEntry {
                    command: plug
                        .command
                        .as_ref()
                        .map(|_| "/usr/local/bin/github-mcp".to_string()),
                    url: plug
                        .url
                        .as_ref()
                        .map(|_| "https://github.example.com/mcp".to_string()),
                    ..plug.clone()
                };
                assert_eq!(imported[0].1, expected, "{target:?}");

                let relinked = layout.link(&written, &entry).unwrap();
                assert_eq!(layout.plug_entry_count(&relinked, &own), 1, "{target:?}");
                let unlinked = layout.unlink(&relinked).unwrap();
                assert!(
                    layout.read_plug(&unlinked).is_none(),
                    "{target:?}: {unlinked}"
                );
            }
        }
    }

    #[test]
    fn a_link_keeps_the_other_servers_and_settings() {
        for (target, existing, other) in [
            (
                ClientTarget::VSCodeCopilot,
                r#"{"servers":{"other":{"command":"x"}}}"#,
                "/servers/other",
            ),
            (
                ClientTarget::Amp,
                r#"{"amp.showCosts":true,"amp.mcpServers":{"other":{"command":"x"}}}"#,
                "/amp.mcpServers/other",
            ),
            (
                ClientTarget::OpenClaw,
                r#"{"gateway":{"port":1},"mcp":{"servers":{"other":{"command":"x"}}}}"#,
                "/mcp/servers/other",
            ),
            (
                ClientTarget::OpenCode,
                r#"{"permission":"allow","mcp":{"other":{"type":"local","command":["x"]}}}"#,
                "/mcp/other",
            ),
            (
                ClientTarget::Nanobot,
                r#"{"tools":{"mcpServers":{"other":{"command":"x"}}}}"#,
                "/tools/mcpServers/other",
            ),
        ] {
            let layout = target.layout();
            let entry = target.plug_entry(LinkTransport::Stdio, "plug", URL);
            let linked = layout.link(existing, &entry).unwrap();
            let value: Value = serde_json::from_str(&linked).unwrap();
            assert!(value.pointer(other).is_some(), "{target:?}: {linked}");
            let unlinked: Value = serde_json::from_str(&layout.unlink(&linked).unwrap()).unwrap();
            assert_eq!(unlinked, serde_json::from_str::<Value>(existing).unwrap());
        }
        // OpenCode refuses a key it does not know.
        let opencode = ClientTarget::OpenCode;
        let linked = opencode
            .layout()
            .link("", &opencode.plug_entry(LinkTransport::Http, "plug", URL))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&linked).unwrap(),
            json!({"mcp": {"plug": {"type": "remote", "url": URL, "enabled": true}}})
        );
    }

    #[test]
    fn a_json_file_with_comments_is_left_alone() {
        let layout = ClientTarget::Amp.layout();
        let commented = "{\n  // my servers\n  \"amp.mcpServers\": {}\n}\n";
        assert!(layout.link(commented, &json!({})).is_err());
        assert!(layout.unlink(commented).is_err());
    }

    #[test]
    fn muse_code_gets_a_schema_version_and_keeps_its_own() {
        let layout = ClientTarget::MuseCode.layout();
        let entry = ClientTarget::MuseCode.plug_entry(LinkTransport::Stdio, "plug", URL);
        let fresh: Value = serde_json::from_str(&layout.link("", &entry).unwrap()).unwrap();
        assert_eq!(fresh["schema_version"], json!(1));
        let kept: Value =
            serde_json::from_str(&layout.link(r#"{"schema_version":2}"#, &entry).unwrap()).unwrap();
        assert_eq!(kept["schema_version"], json!(2));
    }

    #[test]
    fn a_toml_link_is_valid_toml_whatever_the_command() {
        let layout = ClientTarget::CodexCli.layout();
        let command = r#"/Applications/Plug "1".app\Contents/plug"#;
        let entry = ClientTarget::CodexCli.plug_entry(LinkTransport::Stdio, command, URL);
        let linked = layout.link("model = \"o3\"\n", &entry).unwrap();
        let parsed: toml::Table = toml::from_str(&linked).unwrap();
        assert_eq!(
            parsed["mcp_servers"]["plug"]["command"].as_str(),
            Some(command)
        );
        assert_eq!(parsed["model"].as_str(), Some("o3"));
    }

    #[test]
    fn unlinking_toml_keeps_a_server_whose_name_starts_with_plug() {
        let layout = ClientTarget::CodexCli.layout();
        let content = "[mcp_servers.plug]\ncommand = \"plug\"\n\n[mcp_servers.plug.env]\nA = \"1\"\n\n[mcp_servers.plugin]\ncommand = \"plugin\"\n\n[mcp_servers.\"plug\"]\ncommand = \"again\"\n";
        assert_eq!(layout.plug_entry_count(content, &HttpConfig::default()), 2);
        let unlinked = layout.unlink(content).unwrap();
        assert_eq!(unlinked, "[mcp_servers.plugin]\ncommand = \"plugin\"\n\n");
    }

    #[test]
    fn hermes_link_changes_only_the_plug_entry() {
        let layout = ClientTarget::Hermes.layout();
        let existing = "# my notes\nmodel:\n  plug: not a server\nmcp_servers:\n    other:\n        command: other # keep\n\nplatforms:\n  webhook:\n    enabled: true\n";
        let entry = ClientTarget::Hermes.plug_entry(
            LinkTransport::Stdio,
            "/Applications/Plug.app/Contents/Resources/plug",
            URL,
        );
        let linked = layout.link(existing, &entry).unwrap();
        assert!(layout.read_plug(&linked).is_some());
        // Linking twice leaves one entry, and unlinking gives the file back.
        assert_eq!(layout.link(&linked, &entry).unwrap(), linked);
        assert_eq!(layout.unlink(&linked).unwrap(), existing);

        // A file with no servers yet gains the section at its end.
        let fresh = layout.link("model:\n  default: x\n", &entry).unwrap();
        assert!(fresh.starts_with("model:\n  default: x\nmcp_servers:\n  plug:\n"));
        assert!(layout.link("mcp_servers: {}\n", &entry).is_err());
    }

    #[test]
    fn a_hermes_link_written_by_hand_is_read_and_replaced() {
        let layout = ClientTarget::Hermes.layout();
        let existing = "mcp_servers:\n  Plug:\n    command: /Users/rob/.local/bin/plug\n    args:\n      - connect\n    env: {}\nknown:\n  cli:\n    - spotify\n";
        let plug = layout.read_plug(existing).unwrap();
        assert_eq!(plug.args.as_deref(), Some(&["connect".to_string()][..]));
        let entry = ClientTarget::Hermes.plug_entry(LinkTransport::Http, "plug", URL);
        assert_eq!(
            layout.link(existing, &entry).unwrap(),
            format!("mcp_servers:\n  plug:\n    url: {URL}\nknown:\n  cli:\n    - spotify\n")
        );
    }

    #[test]
    fn a_yaml_file_that_is_not_a_mapping_is_left_alone() {
        let layout = ClientTarget::Goose.layout();
        let entry = ClientTarget::Goose.plug_entry(LinkTransport::Stdio, "plug", URL);
        for broken in ["extensions: [unclosed\n", "- a\n- list\n", "just text"] {
            let error = layout.link(broken, &entry).unwrap_err();
            assert!(error.to_string().contains("left it alone"), "{error}");
        }
        let goose = "extensions:\n  github:\n    type: stdio\n    command: npx # mine\n";
        let linked = layout.link(goose, &entry).unwrap();
        assert!(linked.contains("command: npx # mine"));
        assert_eq!(layout.unlink(&linked).unwrap(), goose);
    }

    #[test]
    fn every_plug_entry_is_counted() {
        let count = |target: ClientTarget, content: &str| {
            target
                .layout()
                .plug_entry_count(content, &HttpConfig::default())
        };
        assert_eq!(count(ClientTarget::Goose, "extensions:\n  plug:\n"), 1);
        assert_eq!(
            count(
                ClientTarget::Hermes,
                "model:\n  plug: x\nmcp_servers:\n  Plug:\n  plug:\n"
            ),
            2
        );
        assert_eq!(
            count(
                ClientTarget::Cursor,
                r#"{"mcpServers":{"plug":{},"Plug":{}}}"#
            ),
            2
        );
        assert_eq!(
            count(ClientTarget::OpenClaw, r#"{"mcp":{"servers":{"plug":{}}}}"#),
            1
        );
        assert_eq!(count(ClientTarget::Cursor, "not json at all"), 0);
        assert!(
            ClientTarget::CodexCli
                .layout()
                .check_syntax("[broken")
                .is_err()
        );
    }

    #[test]
    fn set_plug_command_changes_only_the_command_and_keeps_comments() {
        let fixtures = [
            (
                ClientTarget::CodexCli,
                "# doc\n[mcp_servers.plug]\ncommand = \"/opt/homebrew/bin/plug\" # command note\nargs = [\n  \"connect\",\n] # args note\nunknown = \"keep\"\n\n# neighbour\n[mcp_servers.other]\ncommand = \"other\"\n",
            ),
            (
                ClientTarget::Goose,
                "# doc\nextensions:\n  plug:\n    command: /old/plug # command note\n    args:\n\n      - connect\n    unknown: keep # args note\n  # neighbour\n  other:\n    command: other\n",
            ),
        ];
        for (target, content) in fixtures {
            let layout = target.layout();
            let updated = layout
                .set_plug_command(content, "/new/plug", &target.connect_args())
                .unwrap();
            for kept in [
                "# doc",
                "# neighbour",
                "# command note",
                "# args note",
                "unknown",
            ] {
                assert!(updated.contains(kept), "{target:?} lost {kept}: {updated}");
            }
            let plug = layout.read_plug(&updated).unwrap();
            assert_eq!(plug.command.as_deref(), Some("/new/plug"));
            assert_eq!(plug.args.unwrap(), target.connect_args());
            assert_eq!(
                layout
                    .read_servers(&updated, &HttpConfig::default())
                    .unwrap()
                    .len(),
                1
            );
        }
        let opencode = ClientTarget::OpenCode;
        let linked = opencode
            .layout()
            .link(
                "",
                &opencode.plug_entry(LinkTransport::Stdio, "/old/plug", URL),
            )
            .unwrap();
        let moved = opencode
            .layout()
            .set_plug_command(&linked, "/new/plug", &opencode.connect_args())
            .unwrap();
        let value: Value = serde_json::from_str(&moved).unwrap();
        assert_eq!(value["mcp"]["plug"]["command"][0], "/new/plug");
        assert_eq!(value["mcp"]["plug"]["command"][3], "opencode");
    }

    #[test]
    fn arguments_that_are_not_all_strings_are_not_read_as_a_link() {
        let plug = ClientTarget::Cursor
            .layout()
            .read_plug(r#"{"mcpServers":{"plug":{"command":"plug","args":["connect",7]}}}"#)
            .unwrap();
        assert_eq!(plug.args, None);
    }
}
