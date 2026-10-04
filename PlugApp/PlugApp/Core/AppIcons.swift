import AppKit
import SwiftUI

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

    /// Targets that are command line tools. They have no icon to show, and a
    /// terminal glyph says more about them than a generic app square would.
    private static let commandLineTargets: Set<String> = [
        "cline-cli", "gemini-cli", "grok-build", "copilot-cli", "pi",
        "goose", "opencode", "nanobot", "crush", "kimi-code", "qwen-code",
    ]

    /// Clients that live on someone else's servers and reach Plug over the
    /// public address. They have no link target, only a name and a picture.
    private static let remoteTargets: Set<String> = ["gemini", "perplexity", "le-chat"]

    /// Agents a person talks to by text message.
    private static let textAgentTargets: Set<String> = ["poke"]

    /// The symbol that stands in for an app with no icon on this Mac.
    ///
    /// Pure, so the choice can be tested without a filesystem.
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
        if commandLineTargets.contains(key) || text.contains("cli") { return "terminal" }
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
        if let identifier = bundleIdentifiers[target.lowercased()],
           let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: identifier) {
            return NSWorkspace.shared.icon(forFile: url.path)
        }
        // A client Plug has no entry for, started by an app that is right here.
        if let appPath, FileManager.default.fileExists(atPath: appPath) {
            return NSWorkspace.shared.icon(forFile: appPath)
        }
        return installedIcon(named: name)
    }

    /// The icon of the app a server stands for, when this Mac has that app:
    /// a server named "slack" shows Slack's icon.
    @MainActor
    static func image(forServer name: String) -> NSImage? {
        let key = lookupKey(name)
        return installedIcon(named: serverAliases[key] ?? key)
    }

    /// Server names that are not their app's name.
    private static let serverAliases: [String: String] = [
        "imessage": "messages",
        "github": "githubdesktop",
        "gmail": "mail",
        "applenotes": "notes",
        "googledrive": "googledrive",
        "gdrive": "googledrive",
        "linear": "linear",
    ]

    /// A name with everything but its letters and digits removed, so
    /// "agent-admin" finds AgentAdmin.app.
    ///
    /// Pure, so the matching is testable.
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

    @MainActor
    private static func installedIcon(named name: String) -> NSImage? {
        let key = lookupKey(name)
        guard !key.isEmpty, let path = installedApps[key] else { return nil }
        return NSWorkspace.shared.icon(forFile: path)
    }

    /// Match a live session's reported client type to a known target, so a
    /// session row can show the same icon the app row shows.
    ///
    /// Pure, so the matching rules are testable.
    static func target(forClientType clientType: String) -> String {
        let value = clientType
            .lowercased()
            .split(whereSeparator: { !$0.isLetter && !$0.isNumber })
            .joined(separator: "-")
        let compact = value.replacingOccurrences(of: "-", with: "")
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
        // Copilot runs in VS Code and in a terminal. The daemon's own
        // detection splits them the same way.
        if compact.contains("copilot") {
            return compact.contains("cli") ? "copilot-cli" : "vscode"
        }
        // The two xAI clients share a first word. The daemon's own
        // detection splits them the same way.
        if compact.contains("grok") {
            return compact.contains("bot") ? "grok-bot" : "grok-build"
        }
        // Gemini is a terminal client and a remote one.
        if compact.contains("gemini") {
            return compact.contains("cli") ? "gemini-cli" : "gemini"
        }
        if compact.contains("mistral") || compact.contains("lechat") { return "le-chat" }
        if compact.contains("poke") { return "poke" }
        for target in bundleIdentifiers.keys where value.contains(target) { return target }
        for target in commandLineTargets where value.contains(target) { return target }
        return value
    }

    /// The distinct apps behind a set of live sessions, first seen first, so
    /// a row of icons stays stable while sessions come and go.
    ///
    /// Pure, so the grouping is testable.
    static func distinctTargets(forClientTypes clientTypes: [String]) -> [String] {
        var seen = Set<String>()
        return clientTypes.compactMap {
            let target = target(forClientType: $0)
            return seen.insert(target).inserted ? target : nil
        }
    }

    /// Canonical product name for live client sessions. Unknown clients stay
    /// unknown; Plug must not turn an opaque client identifier into a guess.
    static func displayName(forTarget target: String) -> String? {
        switch target.lowercased() {
        case "claude-desktop": return "Claude Desktop"
        case "claude-code": return "Claude Code"
        case "codex", "codex-cli": return "Codex CLI"
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
        case "poke": return "Poke"
        default: return nil
        }
    }
}

/// One app, shown as itself. An app with no icon on this Mac gets a tile
/// with its symbol or its first letter, so every row has a picture of the
/// same size and weight.
struct AppGlyph: View {
    let target: String
    let name: String
    /// The app bundle behind a client with no entry of its own.
    var appPath: String? = nil
    var size: CGFloat = 18

    var body: some View {
        Group {
            if let icon = AppIcons.image(target: target, name: name, appPath: appPath) {
                Image(nsImage: icon)
                    .resizable()
                    .interpolation(.high)
            } else {
                let symbol = AppIcons.symbol(target: target, name: name)
                MonogramTile(
                    name: name.isEmpty ? target : name,
                    symbol: symbol == "app" ? nil : symbol,
                    size: size
                )
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

/// One server, shown as the app it stands for when this Mac has that app,
/// and as a tile with its first letter when it does not.
struct ServerGlyph: View {
    let name: String
    var size: CGFloat = 18
    /// The server's state as a dot on the corner of the icon, the way an
    /// app shows a contact's presence. Nil draws no dot.
    var status: Color?

    var body: some View {
        Group {
            if let icon = AppIcons.image(forServer: name) {
                Image(nsImage: icon)
                    .resizable()
                    .interpolation(.high)
            } else {
                MonogramTile(name: name, size: size)
            }
        }
        .frame(width: size, height: size)
        .overlay(alignment: .bottomTrailing) {
            if let status {
                let dot = max(7, size * 0.32)
                Circle()
                    .fill(status)
                    .frame(width: dot, height: dot)
                    .overlay(Circle().stroke(.background, lineWidth: dot * 0.2))
                    .offset(x: dot * 0.15, y: dot * 0.15)
            }
        }
        .accessibilityHidden(true)
    }
}

/// The stand-in for an icon: a rounded tile carrying a symbol or the first
/// letter of a name. The color comes from the name, so it stays the same
/// from one launch to the next.
struct MonogramTile: View {
    let name: String
    /// A symbol that says what kind of thing this is, shown instead of the
    /// letter on a neutral tile.
    var symbol: String?
    var size: CGFloat = 18

    private static let tints: [Color] = [.blue, .indigo, .purple, .pink, .orange, .teal, .green, .brown]

    /// Which tint a name gets.
    ///
    /// Pure, so it can be tested: the same name always gets the same tint.
    static func tintIndex(for name: String) -> Int {
        let sum = name.lowercased().unicodeScalars.reduce(0) { ($0 &* 31 &+ Int($1.value)) % 1_000_003 }
        return sum % tints.count
    }

    static func letter(for name: String) -> String {
        name.first { $0.isLetter || $0.isNumber }.map { String($0).uppercased() } ?? "?"
    }

    var body: some View {
        // App icons keep a margin inside their square; the tile keeps the
        // same one so the two sit level in a list.
        let side = size * 0.84
        RoundedRectangle(cornerRadius: side * 0.24, style: .continuous)
            .fill(symbol == nil ? AnyShapeStyle(Self.tints[Self.tintIndex(for: name)].gradient) : AnyShapeStyle(Color.gray.gradient))
            .frame(width: side, height: side)
            .overlay {
                if let symbol {
                    Image(systemName: symbol)
                        .font(.system(size: side * 0.5, weight: .semibold))
                        .foregroundStyle(.white)
                } else {
                    Text(Self.letter(for: name))
                        .font(.system(size: side * 0.58, weight: .semibold, design: .rounded))
                        .foregroundStyle(.white)
                }
            }
            .frame(width: size, height: size)
    }
}
