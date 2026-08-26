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
        "cursor": "com.todesktop.230313mzl4w4u92",
        "vscode": "com.microsoft.VSCode",
        "windsurf": "com.exafunction.windsurf",
        "zed": "dev.zed.Zed",
        "antigravity": "com.google.antigravity",
        "junie": "com.jetbrains.junie",
        "goose": "com.block.goose",
    ]

    /// Targets that are command line tools. They have no icon to show, and a
    /// terminal glyph says more about them than a generic app square would.
    private static let commandLineTargets: Set<String> = [
        "claude-code", "codex-cli", "cline-cli", "gemini-cli",
        "goose", "opencode", "nanobot", "crush",
    ]

    /// The symbol that stands in for an app with no icon on this Mac.
    ///
    /// Pure, so the choice can be tested without a filesystem.
    static func symbol(target: String, name: String = "") -> String {
        let key = target.lowercased()
        let text = "\(key) \(name.lowercased())"
        if commandLineTargets.contains(key) || text.contains("cli") { return "terminal" }
        if text.contains("code") || text.contains("cursor") || text.contains("zed") {
            return "chevron.left.forwardslash.chevron.right"
        }
        if text.contains("claude") { return "sparkles" }
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
        let value = clientType.lowercased().replacingOccurrences(of: "_", with: "-")
        if value.contains("claude") && value.contains("code") { return "claude-code" }
        if value.contains("claude") { return "claude-desktop" }
        for target in bundleIdentifiers.keys where value.contains(target) { return target }
        for target in commandLineTargets where value.contains(target) { return target }
        return value
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
