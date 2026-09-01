import Foundation
import PlugIPC

/// A server found in another app's settings, ready to be copied into Plug.
struct DiscoveredServer: Identifiable, Equatable, Sendable {
    var id: String { "\(source)/\(name)" }
    let name: String
    /// The app it was found in, as a linkable target ("cursor", "vscode"), so
    /// the row can show that app's own icon.
    let source: String
    let sourceName: String
    let config: ServerConfig
    /// What it runs, in one line, for the row underneath the name.
    let detail: String
}

/// What a scan of the other apps on this Mac turned up.
struct ImportScan: Equatable, Sendable {
    var servers: [DiscoveredServer] = []
    /// Apps whose settings could not be read, named so the person can see
    /// that the scan was incomplete rather than empty.
    var unreadable: [String] = []

    var isEmpty: Bool { servers.isEmpty }
}

enum ImportError: Error, LocalizedError, Equatable {
    case missingExecutable
    case failed(String)

    var errorDescription: String? {
        switch self {
        case .missingExecutable: "The bundled Plug service is missing."
        case let .failed(message): message
        }
    }
}

protocol ImportScanning: Sendable {
    func scan() async throws -> ImportScan
}

/// Reads the other AI apps' settings files by asking the command line tool,
/// which already knows where each app keeps them.
///
/// The JSON form of `plug import` only ever reports; it writes nothing. That is
/// what makes it safe to run the moment the sheet opens, before anyone has
/// agreed to import anything.
struct ImportService: ImportScanning {
    private let runner: ProcessRunning
    private let executable: URL?

    init(
        runner: ProcessRunning = ProcessRunner(),
        executable: URL? = BundledPlug.executable
    ) {
        self.runner = runner
        self.executable = executable
    }

    func scan() async throws -> ImportScan {
        guard let executable else { throw ImportError.missingExecutable }
        let result = try await runner.run(
            executable: executable,
            arguments: ["import", "--output", "json"],
            timeout: .seconds(20)
        )
        guard !result.stdout.isEmpty else {
            let message = String(data: result.stderr, encoding: .utf8)?
                .trimmingCharacters(in: .whitespacesAndNewlines)
            throw ImportError.failed(
                message?.isEmpty == false ? message! : "Plug could not read the other apps' settings."
            )
        }
        return try ImportScan(json: result.stdout)
    }
}

// MARK: - Reading the report

extension ImportScan {
    /// Built by hand rather than by `Decodable` conformance: the report carries
    /// every field of a server's configuration, and the app only needs the few
    /// it can actually show and re-create.
    init(json data: Data) throws {
        guard let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw ImportError.failed("Plug's answer could not be read.")
        }

        var unreadable: [String] = []
        for scan in root["scanned"] as? [[String: Any]] ?? [] {
            guard let error = scan["error"] as? String, !error.isEmpty else { continue }
            unreadable.append(Self.appName(fromSource: scan["source"] as? String ?? ""))
        }

        let found = root["new_servers"] as? [[String: Any]] ?? []
        let servers = found.compactMap(Self.server(from:)).filter { !$0.config.isPlugItself }

        self.init(servers: servers, unreadable: unreadable.sorted())
    }

    private static func server(from raw: [String: Any]) -> DiscoveredServer? {
        guard let name = raw["name"] as? String, !name.isEmpty,
              let body = raw["config"] as? [String: Any]
        else { return nil }

        let source = raw["source"] as? String ?? ""
        let env = body["env"] as? [String: String] ?? [:]

        var config: ServerConfig
        var detail: String
        if let url = body["url"] as? String, !url.isEmpty {
            config = .remote(url)
            detail = url
        } else if let command = body["command"] as? String, !command.isEmpty {
            let args = body["args"] as? [String] ?? []
            config = .command(command, args: args)
            detail = ([command] + args).joined(separator: " ")
        } else {
            return nil
        }
        config.env = env

        return DiscoveredServer(
            name: name,
            source: target(forSource: source),
            sourceName: appName(fromSource: source),
            config: config,
            detail: detail
        )
    }

    /// `ClaudeDesktop` in the report is `claude-desktop` everywhere else in the
    /// app, which is what picks the icon. Spelled out rather than derived,
    /// because the two names disagree in both directions (`VSCodeCopilot` is
    /// `vscode`, `OpenCode` is `opencode`).
    static func target(forSource source: String) -> String {
        switch source {
        case "ClaudeDesktop": "claude-desktop"
        case "ClaudeCode": "claude-code"
        case "VSCodeCopilot": "vscode"
        case "GeminiCli": "gemini-cli"
        case "CodexCli": "codex-cli"
        case "ClineCli": "cline-cli"
        case "OpenCode": "opencode"
        case "RooCode": "roocode"
        case "Windsurf": "windsurf"
        default: source.lowercased()
        }
    }

    /// The app's name as its own makers write it.
    static func appName(fromSource source: String) -> String {
        switch source {
        case "ClaudeDesktop": "Claude Desktop"
        case "ClaudeCode": "Claude Code"
        case "VSCodeCopilot": "VS Code"
        case "GeminiCli": "Gemini CLI"
        case "CodexCli": "Codex CLI"
        case "ClineCli": "Cline CLI"
        case "OpenCode": "OpenCode"
        case "RooCode": "Roo Code"
        case "Windsurf": "Devin"
        case "": "Another app"
        default: source
        }
    }
}

extension ServerConfig {
    /// Plug's own entry in another app's settings. Every linked app has one,
    /// and importing it would point Plug at itself.
    var isPlugItself: Bool {
        if let command, URL(fileURLWithPath: command).lastPathComponent == "plug" {
            return args.contains("connect") || args.contains("serve")
        }
        return false
    }
}
