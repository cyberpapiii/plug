import AppKit

/// Pictures of the apps people already recognize.
///
/// A row that says "Claude Desktop" has to be read. A row carrying Claude's
/// own icon is recognized before it is read, which is the whole point of a
/// status app: the answer should land at a glance, without a sentence.
///
/// The real icon is used whenever the app can be found on this Mac. When it
/// cannot — the app is not installed, or it is a command line tool with no
/// icon at all — a symbol stands in, chosen so the row still says what kind of
/// thing it is.
enum AppIcons {
    /// Bundle identifiers for the apps Plug can be wired into. Best effort:
    /// a wrong or missing entry costs a symbol instead of an icon, never an
    /// error, and the name lookup below catches most of the rest.
    private static let bundleIdentifiers: [String: String] = [
        "claude-desktop": "com.anthropic.claudefordesktop",
        // Claude Code uses Claude's desktop identity when it has a GUI icon.
        "claude-code": "com.anthropic.claudefordesktop",
        // Codex CLI and the Codex desktop client share OpenAI's installed app
        // identity on macOS (currently shipped as ChatGPT.app).
        "codex": "com.openai.codex",
        "codex-cli": "com.openai.codex",
        "cursor": "com.todesktop.230313mzl4w4u92",
        "vscode": "com.microsoft.VSCode",
        "opencode": "ai.opencode.desktop",
        // Devin.app kept the bundle identifier of the Windsurf app it was.
        "devin": "com.exafunction.windsurf",
        // Grok Bot is a remote client, but its Mac app supplies the icon.
        "grok-bot": "com.anysphere.sand",
        "zed": "dev.zed.Zed",
        "antigravity": "com.google.antigravity",
        "junie": "com.jetbrains.junie",
        "goose": "com.block.goose",
        // Read off Homebrew's casks on 2026-10-04, not off an installed copy.
        "warp": "dev.warp.Warp-Stable",
        "kiro": "dev.kiro.desktop",
        "hermes": "com.nousresearch.hermes",
        // Remote clients whose Mac app supplies the icon. Perplexity's
        // identifier is unchecked; a miss costs a symbol.
        "gemini": "com.google.GeminiMacOS",
        "perplexity": "ai.perplexity.mac",
        "chatgpt": "com.openai.chat",
    ]

    /// A second app that stands for a target. OpenAI ships one Mac app,
    /// ChatGPT.app, under Codex's identifier; the older ChatGPT app had its
    /// own.
    private static let otherBundleIdentifiers: [String: String] = [
        "chatgpt": "com.openai.codex",
    ]

    /// Targets that are command line tools. They have no icon to show, and a
    /// terminal glyph says more about them than a generic app square would.
    private static let commandLineTargets: Set<String> = [
        "cline-cli", "gemini-cli", "grok-build", "copilot-cli", "pi",
        "goose", "opencode", "nanobot", "crush", "kimi-code", "qwen-code",
        "openclaw", "muse-code",
    ]

    /// Clients that live on someone else's servers and reach Plug over the
    /// public address. They have no link target, only a name and a picture.
    private static let remoteTargets: Set<String> = ["gemini", "perplexity", "le-chat", "muse"]

    /// Agents a person talks to by text message.
    private static let textAgentTargets: Set<String> = ["poke"]

    /// The symbol that stands in for an app with no icon on this Mac.
    ///
    static func symbol(target: String, name: String = "") -> String {
        let key = target.lowercased()
        let text = "\(key) \(name.lowercased())"
        // Keep Claude variants recognizable even when Claude.app is not
        // installed. AppIcons.image uses the same installed icon when it is.
        if key == "claude-code" || key == "claude-desktop" || text.contains("claude") {
            return "sparkles"
        }
        // Codex has one visual identity across its CLI and desktop clients.
        // The installed Codex app supplies the official artwork; this is the
        // stable system fallback when that app is absent.
        if key == "codex" || key == "codex-cli" || text.contains("codex") {
            return "app"
        }
        // Goose is optional and often CLI-only. A bird is clearer than a
        // terminal glyph while still remaining a system-provided fallback.
        if key == "goose" || text.contains("goose") {
            return "bird"
        }
        // Amp is a command line tool too. Its name is too short to match on
        // inside another, so it is not in the set above.
        if commandLineTargets.contains(key) || key == "amp" || text.contains("cli") { return "terminal" }
        // An agent reached by text message has no app to show.
        if textAgentTargets.contains(key) { return "message" }
        if remoteTargets.contains(key) { return "globe" }
        if text.contains("code") || text.contains("cursor") || text.contains("zed") {
            return "chevron.left.forwardslash.chevron.right"
        }
        return "app"
    }

    /// The app's real icon, when this Mac has the app.
    @MainActor
    static func image(target: String, name: String, appPath: String? = nil) -> NSImage? {
        let target = target.lowercased()
        for identifier in [bundleIdentifiers[target], otherBundleIdentifiers[target]].compactMap({ $0 }) {
            if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: identifier) {
                return fileIcon(url.path)
            }
        }
        // A client Plug has no entry for, started by an app that is right here.
        if let appPath, FileManager.default.fileExists(atPath: appPath) {
            return fileIcon(appPath)
        }
        return installedIcon(named: name)
    }

    /// The icon of the app a server stands for, when this Mac has that app:
    /// a server named "slack" shows Slack's icon.
    @MainActor
    static func image(forServer name: String) -> NSImage? {
        installedIcon(named: appName(forServer: name) { installedApps[$0] != nil })
    }

    /// The app a command lives inside, when it lives inside one: a server
    /// started from `/Applications/Foo.app/Contents/MacOS/foo-mcp` is Foo.
    ///
    static func appBundle(containing command: String) -> String? {
        guard let end = command.range(of: ".app/") else { return nil }
        return String(command[..<end.lowerBound]) + ".app"
    }

    /// Server names that are not their app's name.
    private static let serverAliases: [String: String] = [
        "imessage": "messages",
        "github": "githubdesktop",
        "applenotes": "notes",
        "gdrive": "googledrive",
        "workspace": "googledrive",
        "googleworkspace": "googledrive",
        "gmail": "googledrive",
    ]

    /// The lookup key of the app a server stands for. Google's servers go
    /// by many names, one per product, and a Mac has an app for only a few
    /// of them, so the rest share Google Drive's icon.
    ///
    static func appName(forServer name: String, installed: (String) -> Bool = { _ in false }) -> String {
        let key = lookupKey(name)
        if let alias = serverAliases[key] { return alias }
        if key.hasPrefix("google"), !installed(key) { return "googledrive" }
        return key
    }

    /// A name with everything but its letters and digits removed, so
    /// "agent-admin" finds AgentAdmin.app.
    ///
    static func lookupKey(_ name: String) -> String {
        String(name.lowercased().unicodeScalars.filter { CharacterSet.alphanumerics.contains($0) })
    }

    /// Where each installed app is, by lookup key. Read once: an app
    /// installed while Plug is open shows its icon after the next launch.
    @MainActor
    private static let installedApps: [String: String] = {
        var found: [String: String] = [:]
        let directories = ["/Applications", "\(NSHomeDirectory())/Applications", "/System/Applications"]
        for directory in directories {
            let names = (try? FileManager.default.contentsOfDirectory(atPath: directory)) ?? []
            for file in names where file.hasSuffix(".app") {
                let key = lookupKey(String(file.dropLast(4)))
                if found[key] == nil { found[key] = "\(directory)/\(file)" }
            }
        }
        return found
    }()

    /// A file's icon, read from disk once. Rows ask for theirs every time
    /// they are drawn.
    @MainActor
    private static let fileIcons = NSCache<NSString, NSImage>()

    @MainActor
    private static func fileIcon(_ path: String) -> NSImage {
        if let kept = fileIcons.object(forKey: path as NSString) { return kept }
        let icon = NSWorkspace.shared.icon(forFile: path)
        fileIcons.setObject(icon, forKey: path as NSString)
        return icon
    }

    @MainActor
    private static func installedIcon(named name: String) -> NSImage? {
        let key = lookupKey(name)
        guard !key.isEmpty, let path = installedApps[key] else { return nil }
        return fileIcon(path)
    }

    /// Match a live session's reported client type to a known target, so a
    /// session row can show the same icon the app row shows.
    ///
    static func target(forClientType clientType: String) -> String {
        let value = clientType
            .lowercased()
            .split(whereSeparator: { !$0.isLetter && !$0.isNumber })
            .joined(separator: "-")
        let compact = value.replacingOccurrences(of: "-", with: "")
        if compact == "dittohistory" || compact == "dittoemailmanager" { return "python" }
        if compact.contains("claudecode") { return "claude-code" }
        if compact.contains("claude") { return "claude-desktop" }
        if compact.contains("codex") {
            return "codex-cli"
        }
        if compact.contains("openai") || compact.contains("chatgpt") { return "chatgpt" }
        if compact.contains("devin") || compact.contains("cascade") ||
            compact.contains("windsurf") || compact.contains("codeium") {
            return "devin"
        }
        // Copilot runs in VS Code and in a terminal.
        if compact.contains("copilot") {
            return compact.contains("cli") ? "copilot-cli" : "vscode"
        }
        // The two xAI clients share a first word.
        if compact.contains("grok") {
            return compact.contains("bot") ? "grok-bot" : "grok-build"
        }
        // Gemini is a terminal client and a remote one.
        if compact.contains("gemini") {
            return compact.contains("cli") ? "gemini-cli" : "gemini"
        }
        if compact.contains("mistral") || compact.contains("lechat") { return "le-chat" }
        if compact.contains("poke") { return "poke" }
        // Meta has two: Muse, its agent, and Muse Code, in a terminal.
        if compact.contains("muse") {
            return compact.contains("code") ? "muse-code" : "muse"
        }
        for target in bundleIdentifiers.keys where value.contains(target) { return target }
        for target in commandLineTargets where value.contains(target) { return target }
        return value
    }

    /// Canonical product name for live client sessions. Unknown clients stay
    /// unknown; Plug must not turn an opaque client identifier into a guess.
    static func displayName(forTarget target: String) -> String? {
        switch target.lowercased() {
        case "claude-desktop": return "Claude Desktop"
        case "claude-code": return "Claude Code"
        case "codex", "codex-cli": return "Codex CLI"
        case "chatgpt": return "ChatGPT"
        case "cursor": return "Cursor"
        case "devin": return "Devin"
        case "copilot-cli": return "GitHub Copilot CLI"
        case "grok-build": return "Grok Build"
        case "grok-bot": return "Grok Bot"
        case "opencode": return "OpenCode"
        case "goose": return "Goose"
        case "warp": return "Warp"
        case "kiro": return "Kiro"
        case "hermes": return "Hermes Agent"
        case "gemini": return "Gemini"
        case "perplexity": return "Perplexity"
        case "le-chat": return "Le Chat"
        case "muse": return "Muse"
        case "muse-code": return "Muse Code"
        case "poke": return "Poke"
        default: return nil
        }
    }
}
