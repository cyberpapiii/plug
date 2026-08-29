@preconcurrency import Foundation

/// One AI app Plug can be wired into, as reported by `plug clients`.
struct LinkableApp: Identifiable, Equatable, Sendable {
    var id: String { target }
    /// Stable identifier the link and unlink commands take (`claude-desktop`).
    let target: String
    /// Name a person recognizes ("Claude Desktop").
    let name: String
    /// Plug is written into this app's MCP configuration.
    let linked: Bool
    /// The app is installed on this Mac.
    let detected: Bool
    /// The app is talking to Plug right now.
    let live: Bool
    let sessions: Int
    /// How a linked app reaches Plug: on this Mac, or over the network.
    let transport: String?

    private enum CodingKeys: String, CodingKey {
        case target, name, linked, detected, live
        case sessions = "live_sessions"
        case transport = "linked_transport"
    }
}

extension LinkableApp: Decodable {
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        target = try container.decode(String.self, forKey: .target)
        name = try container.decodeIfPresent(String.self, forKey: .name) ?? target
        linked = try container.decodeIfPresent(Bool.self, forKey: .linked) ?? false
        detected = try container.decodeIfPresent(Bool.self, forKey: .detected) ?? false
        live = try container.decodeIfPresent(Bool.self, forKey: .live) ?? false
        sessions = try container.decodeIfPresent(Int.self, forKey: .sessions) ?? 0
        transport = try container.decodeIfPresent(String.self, forKey: .transport)
    }
}

enum AppLinkError: LocalizedError, Equatable {
    case serviceMissing
    case commandFailed(String)
    case malformedOutput

    var errorDescription: String? {
        switch self {
        case .serviceMissing: "The bundled Plug service is missing."
        case let .commandFailed(detail): detail
        case .malformedOutput: "Plug returned an app list it could not read."
        }
    }
}

/// Reads and edits which AI apps are wired into Plug.
///
/// This is the one part of the operator surface the daemon does not own: the
/// wiring lives in each client's own configuration file on disk, and the CLI
/// already knows where those are. Running the bundled binary keeps a single
/// implementation rather than a second copy of that knowledge in Swift.
protocol AppLinking: Sendable {
    func apps() async throws -> [LinkableApp]
    func link(target: String) async throws
    func unlink(target: String) async throws
}

struct AppLinkService: AppLinking {
    private let runner: any ProcessRunning
    private let timeout: Duration
    private let executable: URL?

    init(
        runner: any ProcessRunning = ProcessRunner(),
        timeout: Duration = .seconds(30),
        executable: URL? = BundledPlug.executable
    ) {
        self.runner = runner
        self.timeout = timeout
        self.executable = executable
    }

    func apps() async throws -> [LinkableApp] {
        let output = try await run(["clients", "--output", "json"])
        struct Listing: Decodable { let clients: [LinkableApp] }
        do {
            return try JSONDecoder().decode(Listing.self, from: output).clients
        } catch {
            throw AppLinkError.malformedOutput
        }
    }

    func link(target: String) async throws {
        _ = try await run(["link", target, "--yes", "--output", "json"])
    }

    func unlink(target: String) async throws {
        _ = try await run(["unlink", target, "--yes", "--output", "json"])
    }

    @discardableResult
    private func run(_ arguments: [String]) async throws -> Data {
        guard let executable else { throw AppLinkError.serviceMissing }
        let result = try await runner.run(
            executable: executable,
            arguments: arguments,
            timeout: timeout
        )
        guard result.status == 0 else {
            let detail = String(data: result.stderr, encoding: .utf8)?
                .trimmingCharacters(in: .whitespacesAndNewlines)
            throw AppLinkError.commandFailed(
                detail.flatMap { $0.isEmpty ? nil : $0 } ?? "Plug could not update that app."
            )
        }
        return result.stdout
    }
}
