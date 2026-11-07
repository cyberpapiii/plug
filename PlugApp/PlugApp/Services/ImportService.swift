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

enum ImportError: Error, LocalizedError, AdvisedError, Equatable {
    case missingExecutable
    case failed(String)

    var errorDescription: String? {
        switch self {
        case .missingExecutable: "Plug is damaged. Download it again and replace this copy."
        case let .failed(message): message
        }
    }

    var advice: String {
        if case let .failed(message) = self { return Explain.advice(forOutside: message) }
        return Explain.fallback
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
    /// The parts of `plug import --output json` the sheet reads. Each server
    /// keeps its whole configuration, so what is imported is what was found.
    fileprivate struct Report: Decodable {
        struct Scanned: Decodable {
            let sourceName: String
            let error: String?
        }

        struct Found: Decodable {
            let name: String
            let sourceTarget: String
            let sourceName: String
            let config: ServerConfig
        }

        let scanned: [Scanned]
        let newServers: [Found]
    }

    init(json data: Data) throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        guard let report = try? decoder.decode(Report.self, from: data) else {
            throw ImportError.failed("Plug gave an answer it could not read. Try again.")
        }
        let unreadable = report.scanned.filter { !($0.error ?? "").isEmpty }.map(\.sourceName)
        // `plug import` has already left out Plug's own entries.
        let servers = report.newServers.compactMap(DiscoveredServer.init)
        self.init(servers: servers, unreadable: unreadable.sorted())
    }
}

private extension DiscoveredServer {
    /// Nil for an entry with no name, or nothing to run or reach.
    init?(_ found: ImportScan.Report.Found) {
        let config = found.config
        let detail: String
        if let url = config.url, !url.isEmpty {
            detail = url
        } else if let command = config.command, !command.isEmpty {
            detail = ([command] + config.args).joined(separator: " ")
        } else {
            return nil
        }
        guard !found.name.isEmpty else { return nil }
        self.init(
            name: found.name,
            source: found.sourceTarget,
            sourceName: found.sourceName,
            config: config,
            detail: detail
        )
    }
}
