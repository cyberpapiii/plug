import Foundation
import PlugIPC

/// One line of the "here's what I understood" preview.
struct DraftFact: Equatable, Sendable, Identifiable {
    var id: String { label }
    let label: String
    let value: String
}

/// A server the app has understood from pasted text but has not saved yet.
struct ServerDraft: Equatable, Sendable {
    var name: String
    var config: ServerConfig
    var facts: [DraftFact]
}

enum ServerDraftParse: Equatable, Sendable {
    case empty
    case draft(ServerDraft)
    case unreadable(String)
}

/// Turns whatever someone pasted into a server definition.
///
/// People acquire MCP servers by copying from a README, so the three things
/// worth accepting are the JSON block those READMEs print, a bare URL, and the
/// shell command they would otherwise have run. Anything else is refused with a
/// sentence that says what to paste instead.
enum ServerDraftParser {
    static func parse(_ raw: String) -> ServerDraftParse {
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return .empty }

        if text.hasPrefix("{") {
            return parseJSON(text)
        }
        if let url = remoteURL(in: text) {
            return .draft(remoteDraft(name: suggestedName(forHost: url), url: url))
        }
        return parseCommand(text)
    }

    // MARK: - JSON

    private static func parseJSON(_ text: String) -> ServerDraftParse {
        guard let data = text.data(using: .utf8),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            return .unreadable("That looks like JSON, but it isn't complete. Copy the whole block, including both braces.")
        }

        // Editors' config files wrap entries in "mcpServers" (or "servers").
        let container = (root["mcpServers"] as? [String: Any])
            ?? (root["servers"] as? [String: Any])
            ?? root

        if looksLikeServerBody(container) {
            let name = (container["name"] as? String) ?? "New server"
            guard let draft = draft(named: name, from: container) else {
                return .unreadable("That entry has no command or url, so there's nothing to connect to.")
            }
            return .draft(draft)
        }

        let entries = container.compactMap { key, value -> ServerDraft? in
            guard let body = value as? [String: Any] else { return nil }
            return draft(named: key, from: body)
        }
        .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }

        guard let first = entries.first else {
            return .unreadable("No server entries in there. Paste the block that has a command or url inside it.")
        }
        return .draft(first)
    }

    private static func looksLikeServerBody(_ body: [String: Any]) -> Bool {
        body["command"] != nil || body["url"] != nil || body["type"] != nil || body["transport"] != nil
    }

    private static func draft(named name: String, from body: [String: Any]) -> ServerDraft? {
        if let url = (body["url"] as? String)?.trimmingCharacters(in: .whitespaces), !url.isEmpty {
            var config = ServerConfig.remote(url)
            if let declared = declaredTransport(in: body), declared == "sse" {
                config.transport = "sse"
            }
            if let token = bearerToken(in: body) {
                config.authToken = token
            }
            return ServerDraft(name: name, config: config, facts: facts(for: config))
        }

        guard let command = (body["command"] as? String)?.trimmingCharacters(in: .whitespaces),
              !command.isEmpty
        else { return nil }

        let args = (body["args"] as? [Any])?.compactMap { $0 as? String } ?? []
        var config = ServerConfig.command(command, args: args)
        if let env = body["env"] as? [String: Any] {
            config.env = env.compactMapValues { $0 as? String }
        }
        if let enabled = body["enabled"] as? Bool {
            config.enabled = enabled
        }
        return ServerDraft(name: name, config: config, facts: facts(for: config))
    }

    private static func declaredTransport(in body: [String: Any]) -> String? {
        let raw = (body["type"] as? String) ?? (body["transport"] as? String)
        return raw?.lowercased()
    }

    private static func bearerToken(in body: [String: Any]) -> String? {
        guard let headers = body["headers"] as? [String: Any] else { return nil }
        for (key, value) in headers where key.lowercased() == "authorization" {
            guard let text = value as? String else { continue }
            if text.lowercased().hasPrefix("bearer ") {
                return String(text.dropFirst("bearer ".count))
            }
            return text
        }
        return nil
    }

    // MARK: - URL

    private static func remoteURL(in text: String) -> String? {
        guard text.hasPrefix("http://") || text.hasPrefix("https://") else { return nil }
        let candidate = text.split(whereSeparator: \.isWhitespace).first.map(String.init) ?? text
        guard URL(string: candidate)?.host != nil else { return nil }
        return candidate
    }

    private static func remoteDraft(name: String, url: String) -> ServerDraft {
        let config = ServerConfig.remote(url)
        return ServerDraft(name: name, config: config, facts: facts(for: config))
    }

    private static func suggestedName(forHost url: String) -> String {
        guard let host = URL(string: url)?.host else { return "New server" }
        let stripped = host.hasPrefix("www.") ? String(host.dropFirst(4)) : host
        let label = stripped.split(separator: ".").first.map(String.init) ?? stripped
        return label.isEmpty ? "New server" : label
    }

    // MARK: - Shell command

    private static func parseCommand(_ text: String) -> ServerDraftParse {
        var tokens = tokenize(text)
        guard !tokens.isEmpty else {
            return .unreadable("Paste a command, a URL, or the JSON block from the server's instructions.")
        }

        var env: [String: String] = [:]
        while let first = tokens.first, let split = environmentAssignment(first) {
            env[split.0] = split.1
            tokens.removeFirst()
        }

        guard let command = tokens.first else {
            return .unreadable("That's only environment variables. Add the command that starts the server.")
        }

        var config = ServerConfig.command(command, args: Array(tokens.dropFirst()))
        config.env = env
        return .draft(
            ServerDraft(name: suggestedName(forCommand: command, args: config.args), config: config, facts: facts(for: config))
        )
    }

    private static func environmentAssignment(_ token: String) -> (String, String)? {
        guard let index = token.firstIndex(of: "="), index != token.startIndex else { return nil }
        let key = String(token[token.startIndex..<index])
        guard key.allSatisfy({ $0.isUppercase || $0.isNumber || $0 == "_" }) else { return nil }
        return (key, String(token[token.index(after: index)...]))
    }

    /// The package name is a better guess at what someone calls this server than
    /// the runner that launches it, so skip runners and their flags.
    private static func suggestedName(forCommand command: String, args: [String]) -> String {
        let runners: Set<String> = ["npx", "uvx", "bunx", "pnpm", "npm", "yarn", "uv", "pipx", "deno", "node", "python", "python3"]
        let base = command.split(separator: "/").last.map(String.init) ?? command
        guard runners.contains(base) else { return tidy(base) }
        for argument in args where !argument.hasPrefix("-") {
            let candidate = argument.split(separator: "/").last.map(String.init) ?? argument
            let withoutVersion = candidate.split(separator: "@").first.map(String.init) ?? candidate
            guard !withoutVersion.isEmpty else { continue }
            return tidy(withoutVersion)
        }
        return tidy(base)
    }

    private static func tidy(_ value: String) -> String {
        let cleaned = value
            .replacingOccurrences(of: "mcp-server-", with: "")
            .replacingOccurrences(of: "-mcp-server", with: "")
            .replacingOccurrences(of: "-mcp", with: "")
            .replacingOccurrences(of: "mcp-", with: "")
            .replacingOccurrences(of: ".py", with: "")
            .replacingOccurrences(of: ".js", with: "")
        return cleaned.isEmpty ? value : cleaned
    }

    /// Quote-aware split, so pasted commands with quoted paths survive.
    static func tokenize(_ text: String) -> [String] {
        var tokens: [String] = []
        var current = ""
        var quote: Character?
        var hasContent = false
        var escaping = false

        for character in text {
            if escaping {
                current.append(character)
                hasContent = true
                escaping = false
                continue
            }
            if let active = quote {
                if character == active {
                    quote = nil
                } else {
                    current.append(character)
                }
                continue
            }
            switch character {
            case "\\":
                escaping = true
                hasContent = true
            case "\"", "'":
                quote = character
                hasContent = true
            case " ", "\t", "\n", "\r":
                if hasContent {
                    tokens.append(current)
                    current = ""
                    hasContent = false
                }
            default:
                current.append(character)
                hasContent = true
            }
        }
        if escaping { current.append("\\") }
        if hasContent { tokens.append(current) }
        return tokens
    }

    // MARK: - Preview

    private static func facts(for config: ServerConfig) -> [DraftFact] {
        var facts: [DraftFact] = []
        switch config.transport {
        case "http", "sse":
            facts.append(DraftFact(label: "Connects to", value: config.url ?? "—"))
            facts.append(DraftFact(label: "Kind", value: "Remote server"))
            if config.authToken != nil {
                facts.append(DraftFact(label: "Authorization", value: "Token included"))
            }
        default:
            let invocation = ([config.command ?? ""] + config.args).joined(separator: " ")
            facts.append(DraftFact(label: "Runs", value: invocation.trimmingCharacters(in: .whitespaces)))
            facts.append(DraftFact(label: "Kind", value: "Runs on this Mac"))
            if !config.env.isEmpty {
                let names = config.env.keys.sorted().joined(separator: ", ")
                facts.append(DraftFact(label: "Environment", value: names))
            }
        }
        return facts
    }
}
