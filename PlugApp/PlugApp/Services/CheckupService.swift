@preconcurrency import Foundation

/// One thing that was checked, in words a person can act on.
struct Check: Identifiable, Equatable, Sendable, Decodable {
    enum Result: String, Equatable, Sendable {
        case pass, warn, fail
    }

    var id: String { name }
    let name: String
    let result: Result
    let message: String
    /// What the runtime suggests doing about it, when it has a suggestion.
    let fix: String?

    private enum CodingKeys: String, CodingKey {
        case name, status, message
        case fix = "fix_suggestion"
    }

    init(name: String, result: Result, message: String, fix: String? = nil) {
        self.name = name
        self.result = result
        self.message = message
        self.fix = fix
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        name = try container.decode(String.self, forKey: .name)
        message = try container.decodeIfPresent(String.self, forKey: .message) ?? ""
        fix = try container.decodeIfPresent(String.self, forKey: .fix)
        let status = try container.decodeIfPresent(String.self, forKey: .status)?.lowercased()
        result = switch status {
        case "pass", "ok": .pass
        case "warn", "warning": .warn
        default: .fail
        }
    }

    /// The check's own name is an identifier (`config_permissions`). This is
    /// what the row says instead.
    var title: String {
        switch name {
        case "config_exists": "Settings file"
        case "config_permissions": "Settings file is private"
        case "port_available": "Network port"
        case "env_vars": "Server passwords and keys"
        case "server_binaries": "Server programs"
        case "tool_collisions": "Tool names"
        case "client_limits": "How many tools apps can take"
        case "pid_staleness", "runtime_health": "Background service"
        case "client_configs": "App setup"
        case "server_connectivity": "Servers responding"
        case "http_auth", "downstream_oauth_owner": "Remote access"
        case "oauth_config", "oauth_tokens": "Stored sign-ins"
        case "codesign_identity": "App signature"
        case "unified_install": "Installation"
        default: name.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }
}

/// Everything that was checked, and whether any of it needs a person.
struct Checkup: Equatable, Sendable, Decodable {
    let checks: [Check]

    var problems: [Check] { checks.filter { $0.result == .fail } }
    var warnings: [Check] { checks.filter { $0.result == .warn } }
    var isClean: Bool { problems.isEmpty && warnings.isEmpty }

    /// One line for the top of the panel, in the same plain register the rest
    /// of the app uses.
    var headline: String {
        if checks.isEmpty { return "Nothing was checked" }
        if isClean { return "All \(checks.count) checks passed" }
        var parts: [String] = []
        if !problems.isEmpty {
            parts.append(problems.count == 1 ? "1 problem" : "\(problems.count) problems")
        }
        if !warnings.isEmpty {
            parts.append(warnings.count == 1 ? "1 warning" : "\(warnings.count) warnings")
        }
        return parts.joined(separator: ", ")
    }

    /// Trouble first, because that is what the checkup was run for.
    var ordered: [Check] {
        problems + warnings + checks.filter { $0.result == .pass }
    }
}

enum CheckupError: LocalizedError, Equatable {
    case serviceMissing
    case unreadable

    var errorDescription: String? {
        switch self {
        case .serviceMissing: "The bundled Plug service is missing."
        case .unreadable: "Plug returned a checkup it could not read."
        }
    }
}

protocol CheckupRunning: Sendable {
    func run() async throws -> Checkup
    /// Where the settings file lives, for the button that reveals it.
    func configPath() async -> URL?
}

/// Runs `plug doctor` and reads the result.
///
/// The checks themselves live in the runtime, where they can see the config,
/// the port, the launchd jobs and the servers. The app asks the same question
/// the terminal asks and shows the answer in rows instead of lines.
struct CheckupService: CheckupRunning {
    private let runner: any ProcessRunning
    private let executable: URL?
    private let timeout: Duration

    init(
        runner: any ProcessRunning = ProcessRunner(),
        timeout: Duration = .seconds(60),
        executable: URL? = Bundle.main.url(forResource: "plug", withExtension: nil)
    ) {
        self.runner = runner
        self.timeout = timeout
        self.executable = executable
    }

    func run() async throws -> Checkup {
        guard let executable else { throw CheckupError.serviceMissing }
        // A checkup that finds problems exits non-zero. That is the answer, not
        // a failure, so the exit status is deliberately not consulted.
        let result = try await runner.run(
            executable: executable,
            arguments: ["doctor", "--output", "json"],
            timeout: timeout
        )
        do {
            return try JSONDecoder().decode(Checkup.self, from: result.stdout)
        } catch {
            throw CheckupError.unreadable
        }
    }

    func configPath() async -> URL? {
        guard let executable else { return nil }
        guard let result = try? await runner.run(
            executable: executable,
            arguments: ["config", "path"],
            timeout: .seconds(15)
        ) else { return nil }
        let text = String(data: result.stdout, encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return text.isEmpty ? nil : URL(filePath: text)
    }
}
