import Foundation
@testable import Plug

/// A command line that answers from a closure and remembers what it was asked.
actor StubProcessRunner: ProcessRunning {
    struct Call: Equatable, Sendable {
        let executable: URL
        let arguments: [String]
        let timeout: Duration
    }

    private(set) var calls: [Call] = []
    /// Runs that ended because the calling task was cancelled.
    private(set) var cancellations = 0
    private let answer: @Sendable (_ executable: URL, _ arguments: [String]) async throws -> ProcessResult

    init(_ answer: @escaping @Sendable (_ executable: URL, _ arguments: [String]) async throws -> ProcessResult) {
        self.answer = answer
    }

    init(result: ProcessResult) {
        self.init { _, _ in result }
    }

    init(error: any Error) {
        self.init { _, _ in throw error }
    }

    var arguments: [[String]] { calls.map(\.arguments) }

    func run(executable: URL, arguments: [String], timeout: Duration) async throws -> ProcessResult {
        calls.append(Call(executable: executable, arguments: arguments, timeout: timeout))
        do {
            return try await answer(executable, arguments)
        } catch is CancellationError {
            cancellations += 1
            throw CancellationError()
        }
    }
}

extension ProcessResult {
    static func exited(_ status: Int32 = 0, stdout: String = "", stderr: String = "") -> ProcessResult {
        ProcessResult(status: status, stdout: Data(stdout.utf8), stderr: Data(stderr.utf8))
    }
}
