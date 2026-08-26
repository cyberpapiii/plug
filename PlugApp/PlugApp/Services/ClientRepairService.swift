import Foundation

struct ClientRepairResult: Codable, Equatable, Sendable {
    let examined: Int
    let repaired: Int
    let unchanged: Int
}

enum ClientRepairError: Error, Equatable {
    case commandFailed(status: Int32, stderr: String)
    case malformedOutput
}

protocol ClientRepairing: Sendable {
    func inspect(canonicalExecutable: URL) async throws -> Bool
    func repairAll(canonicalExecutable: URL) async throws -> ClientRepairResult
}

struct ClientRepairService: ClientRepairing {
    private static let inspectArguments = ["doctor", "--output", "json"]
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
        // Doctor uses 0 for healthy, 1 for failed checks, and 2 for warnings.
        // All three still carry the same machine-readable inspection report.
        guard (0...2).contains(process.status) else {
            throw commandFailed(for: process)
        }

        do {
            return try JSONDecoder().decode(ParsedDoctorReport.self, from: process.stdout)
                .unifiedInstall
                .clientRepairNeeded
        } catch {
            throw ClientRepairError.malformedOutput
        }
    }

    func repairAll(canonicalExecutable: URL) async throws -> ClientRepairResult {
        try await executeRepair(canonicalExecutable: canonicalExecutable).result
    }

    private func executeRepair(canonicalExecutable: URL) async throws -> ParsedRepairReport {
        let process = try await runner.run(
            executable: canonicalExecutable,
            arguments: Self.repairArguments,
            timeout: timeout
        )
        guard process.status == 0 else {
            throw commandFailed(for: process)
        }

        do {
            return try JSONDecoder().decode(ParsedRepairReport.self, from: process.stdout)
        } catch {
            throw ClientRepairError.malformedOutput
        }
    }

    private func commandFailed(for process: ProcessResult) -> ClientRepairError {
        .commandFailed(
            status: process.status,
            stderr: String(decoding: process.stderr, as: UTF8.self)
                .trimmingCharacters(in: .whitespacesAndNewlines)
        )
    }
}

private struct ParsedDoctorReport: Decodable {
    let unifiedInstall: ParsedUnifiedInstall

    private enum CodingKeys: String, CodingKey {
        case unifiedInstall = "unified_install"
    }
}

private struct ParsedUnifiedInstall: Decodable {
    let clientRepairNeeded: Bool

    private enum CodingKeys: String, CodingKey {
        case clientRepairNeeded = "client_repair_needed"
    }
}

private struct ParsedRepairReport: Decodable {
    let items: [ParsedRepairItem]

    var result: ClientRepairResult {
        let repaired = items.count(where: \.changed)
        return ClientRepairResult(
            examined: items.count,
            repaired: repaired,
            unchanged: items.count - repaired
        )
    }

    private enum CodingKeys: String, CodingKey {
        case items
    }
}

private struct ParsedRepairItem: Decodable {
    let changed: Bool

    private enum CodingKeys: String, CodingKey {
        case changed
    }
}
