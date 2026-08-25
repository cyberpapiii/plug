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
    private static let arguments = ["repair", "--all", "--output", "json"]

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
        try await execute(canonicalExecutable: canonicalExecutable).needsRepair
    }

    func repairAll(canonicalExecutable: URL) async throws -> ClientRepairResult {
        try await execute(canonicalExecutable: canonicalExecutable).result
    }

    private func execute(canonicalExecutable: URL) async throws -> ParsedRepairReport {
        let process = try await runner.run(
            executable: canonicalExecutable,
            arguments: Self.arguments,
            timeout: timeout
        )
        guard process.status == 0 else {
            throw ClientRepairError.commandFailed(
                status: process.status,
                stderr: String(decoding: process.stderr, as: UTF8.self)
                    .trimmingCharacters(in: .whitespacesAndNewlines)
            )
        }

        do {
            return try JSONDecoder().decode(ParsedRepairReport.self, from: process.stdout)
        } catch {
            throw ClientRepairError.malformedOutput
        }
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

    var needsRepair: Bool {
        items.contains { item in
            if item.changed { return true }
            switch item.disposition {
            case "Canonical", "Http", "Missing", nil:
                return false
            default:
                return true
            }
        }
    }

    private enum CodingKeys: String, CodingKey {
        case items
    }
}

private struct ParsedRepairItem: Decodable {
    let changed: Bool
    let disposition: String?

    private enum CodingKeys: String, CodingKey {
        case changed
        case disposition
    }
}
