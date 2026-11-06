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
        case .missingExecutable: "Plug is damaged. Download it again and replace this copy."
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
                message?.isEmpty == false ? message! : "Plug could not read your other clients' settings."
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
            throw ImportError.failed("Plug gave an answer it could not read. Try again.")
        }

        var unreadable: [String] = []
        for scan in root["scanned"] as? [[String: Any]] ?? [] {
            guard let error = scan["error"] as? String, !error.isEmpty else { continue }
            unreadable.append(scan["source_name"] as? String ?? "Another client")
        }

        let found = root["new_servers"] as? [[String: Any]] ?? []
        let servers = found.compactMap(Self.server(from:)).filter { !$0.config.isPlugItself }

        self.init(servers: servers, unreadable: unreadable.sorted())
    }

    private static func server(from raw: [String: Any]) -> DiscoveredServer? {
        guard let name = raw["name"] as? String, !name.isEmpty,
              let body = raw["config"] as? [String: Any]
        else { return nil }

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
            source: raw["source_target"] as? String ?? "",
            sourceName: raw["source_name"] as? String ?? "Another client",
            config: config,
            detail: detail
        )
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
