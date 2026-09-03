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
        // Cognition's Devin app retains this bundle identifier for backward
        // compatibility with the former Windsurf desktop app.
        "windsurf": "com.exafunction.windsurf",
        "zed": "dev.zed.Zed",
        "antigravity": "com.google.antigravity",
        "junie": "com.jetbrains.junie",
        "goose": "com.block.goose",
    ]

    /// Targets that are command line tools. They have no icon to show, and a
    /// terminal glyph says more about them than a generic app square would.
    private static let commandLineTargets: Set<String> = [
        "cline-cli", "gemini-cli",
        "goose", "opencode", "nanobot", "crush",
    ]

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
            return "app.dashed"
        }
        // Goose is optional and often CLI-only. A bird is clearer than a
        // terminal glyph while still remaining a system-provided fallback.
        if key == "goose" || text.contains("goose") {
            return "bird.fill"
        }
        if commandLineTargets.contains(key) || text.contains("cli") { return "terminal" }
        if text.contains("code") || text.contains("cursor") || text.contains("zed") {
            return "chevron.left.forwardslash.chevron.right"
        }
        return "app.dashed"
    }

    /// The app's real icon, when this Mac has the app.
    @MainActor
    static func image(target: String, name: String) -> NSImage? {
        if let identifier = bundleIdentifiers[target.lowercased()],
           let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: identifier) {
            return NSWorkspace.shared.icon(forFile: url.path)
        }
        guard !name.isEmpty else { return nil }
        for directory in ["/Applications", "\(NSHomeDirectory())/Applications"] {
            let path = "\(directory)/\(name).app"
            if FileManager.default.fileExists(atPath: path) {
                return NSWorkspace.shared.icon(forFile: path)
            }
        }
        return nil
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
        if compact.contains("devin") || compact.contains("cascade") ||
            compact.contains("windsurf") || compact.contains("codeium") {
            return "windsurf"
        }
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
        case "windsurf": return "Devin"
        case "opencode": return "OpenCode"
        case "goose": return "Goose"
        default: return nil
        }
    }
}

/// One app, shown as itself.
struct AppGlyph: View {
    let target: String
    let name: String
    var size: CGFloat = 22

    var body: some View {
        Group {
            if let icon = AppIcons.image(target: target, name: name) {
                Image(nsImage: icon)
                    .resizable()
                    .interpolation(.high)
            } else {
                Image(systemName: AppIcons.symbol(target: target, name: name))
                    .font(.system(size: size * 0.62))
                    .foregroundStyle(.secondary)
                    .frame(width: size, height: size)
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}
