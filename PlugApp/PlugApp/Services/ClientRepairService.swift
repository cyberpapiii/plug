import Foundation

enum ClientRepairError: Error, Equatable {
    case commandFailed(status: Int32, stderr: String)
    case malformedOutput
}

protocol ClientRepairing: Sendable {
    func inspect(canonicalExecutable: URL) async throws -> Bool
    func repairAll(canonicalExecutable: URL) async throws
}

struct ClientRepairService: ClientRepairing {
    private static let inspectArguments = ["repair", "--all", "--dry-run", "--output", "json"]
    private static let repairArguments = ["repair", "--all", "--output", "json"]

    private let runner: any ProcessRunning
    private let timeout: Duration

    init(
        runner: any ProcessRunning = ProcessRunner(),
        timeout: Duration = .seconds(60)
    ) {
        self.runner = runner
        self.timeout = timeout
    }

    func inspect(canonicalExecutable: URL) async throws -> Bool {
        let process = try await runner.run(
            executable: canonicalExecutable,
            arguments: Self.inspectArguments,
            timeout: timeout
        )
        guard process.status == 0 else {
            throw commandFailed(for: process)
        }

        do {
            return try JSONDecoder().decode(ParsedRepairReport.self, from: process.stdout).repairNeeded
        } catch {
            throw ClientRepairError.malformedOutput
        }
    }

    /// A report that does not parse means the repair cannot be trusted to
    /// have run, so it fails like the command did.
    func repairAll(canonicalExecutable: URL) async throws {
        let process = try await runner.run(
            executable: canonicalExecutable,
            arguments: Self.repairArguments,
            timeout: timeout
        )
        guard process.status == 0 else {
            throw commandFailed(for: process)
        }

        do {
            _ = try JSONDecoder().decode(ParsedRepairReport.self, from: process.stdout)
        } catch {
            throw ClientRepairError.malformedOutput
        }
    }

    private func commandFailed(for process: ProcessResult) -> ClientRepairError {
        .commandFailed(
            status: process.status,
            stderr: process.stderrText
        )
    }
}

/// The CLI's own verdict on its report, so the rule for which links need
/// repair lives only in `PlugLinkDisposition::needs_repair`.
private struct ParsedRepairReport: Decodable {
    let repairNeeded: Bool

    private enum CodingKeys: String, CodingKey {
        case repairNeeded = "repair_needed"
    }
}
