import Foundation

/// Which client something is, by its link target.
///
/// The daemon names every client it can link, by the slug in its registry
/// (`plug-core/src/clients`, pinned in `testdata/clients.json`), on sessions,
/// calls and sign-ins. What it cannot name is left to `guess(from:)`: remote
/// products with no registry entry (ChatGPT, Grok Bot, Perplexity, Le Chat,
/// Muse, Poke), and names the owner typed.
enum ClientIdentity {
    /// The product name of each client, by slug. The registry's names come
    /// first, then the remote products only the app pictures.
    private static let names: [String: String] = [
        "claude-desktop": "Claude Desktop",
        "claude-code": "Claude Code",
        "cursor": "Cursor",
        "devin": "Devin",
        "vscode": "VS Code Copilot",
        "copilot-cli": "GitHub Copilot CLI",
        "gemini-cli": "Gemini CLI",
        "codex-cli": "Codex CLI",
        "grok-build": "Grok Build",
        "opencode": "OpenCode",
        "zed": "Zed",
        "cline": "Cline (VS Code)",
        "cline-cli": "Cline CLI",
        "roocode": "RooCode",
        "factory": "Factory",
        "nanobot": "Nanobot",
        "junie": "JetBrains Junie",
        "kilo": "Kilo Code",
        "pi": "Pi",
        "warp": "Warp",
        "kiro": "Kiro",
        "kimi-code": "Kimi Code",
        "qwen-code": "Qwen Code",
        "antigravity": "Google Antigravity",
        "goose": "Goose",
        "hermes": "Hermes Agent",
        "amp": "Amp",
        "openclaw": "OpenClaw",
        "lm-studio": "LM Studio",
        "muse-code": "Muse Code",
    ]

    /// Clients that reach Plug only over the network, so the daemon has no
    /// registry entry to name them by.
    static let remoteNames: [String: String] = [
        "chatgpt": "ChatGPT",
        "grok-bot": "Grok Bot",
        "perplexity": "Perplexity",
        "le-chat": "Le Chat",
        "muse": "Muse",
        "poke": "Poke",
    ]

    /// Every slug the registry has a name for, as the app spells it.
    static var registryNames: [String: String] { names }

    /// The product name of a client Plug knows. Nil for any other, so an
    /// opaque identifier is never turned into a guess.
    static func displayName(forTarget target: String) -> String? {
        let key = target.lowercased()
        return names[key] ?? remoteNames[key]
    }

    /// A known client a name stands for, nil when it stands for none.
    static func knownTarget(named name: String?) -> String? {
        guard let name, !name.isEmpty else { return nil }
        let target = guess(from: name)
        return displayName(forTarget: target) == nil ? nil : target
    }

    /// Which client to show, best witness first: the one its owner named it
    /// as, the one the daemon took it for, a product Plug knows by the name
    /// it reports, then the name on its sign-in.
    static func resolve(
        ownerName: String?,
        daemonTarget: String?,
        reported: String?,
        grantName: String?
    ) -> String {
        if let named = knownTarget(named: ownerName) { return named }
        if let daemonTarget { return daemonTarget }
        if let known = knownTarget(named: reported) { return known }
        if let grantName, !grantName.isEmpty { return guess(from: grantName) }
        return guess(from: reported ?? "")
    }

    /// The client a free-text name most likely is. Only for names the daemon
    /// did not place.
    static func guess(from text: String) -> String {
        let value = text
            .lowercased()
            .split(whereSeparator: { !$0.isLetter && !$0.isNumber })
            .joined(separator: "-")
        let compact = value.replacingOccurrences(of: "-", with: "")
        if compact.contains("claudecode") { return "claude-code" }
        if compact.contains("claude") { return "claude-desktop" }
        if compact.contains("codex") { return "codex-cli" }
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
        if compact.contains("gemini") { return "gemini-cli" }
        if compact.contains("mistral") || compact.contains("lechat") { return "le-chat" }
        if compact.contains("poke") { return "poke" }
        // Meta has two: Muse, its agent, and Muse Code, in a terminal.
        if compact.contains("muse") {
            return compact.contains("code") ? "muse-code" : "muse"
        }
        // Whole words only: "pi" is not in "api".
        let words = "-\(value)-"
        let slugs = names.keys.sorted { $0.count > $1.count }
        for slug in slugs where words.contains("-\(slug)-") { return slug }
        return value
    }
}
