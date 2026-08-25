import Foundation
import Darwin

struct ProcessResult: Sendable {
    let status: Int32
    let stdout: Data
    let stderr: Data
}

enum ProcessRunnerError: Error, Equatable {
    case timedOut
}

protocol ProcessRunning: Sendable {
    func run(executable: URL, arguments: [String], timeout: Duration) async throws -> ProcessResult
}

struct ProcessRunner: ProcessRunning {
    func run(
        executable: URL,
        arguments: [String],
        timeout: Duration
    ) async throws -> ProcessResult {
        try await withCheckedThrowingContinuation { continuation in
            let execution = ProcessExecution(
                executable: executable,
                arguments: arguments,
                continuation: continuation
            )
            execution.start(timeout: timeout)
        }
    }
}

private final class ProcessExecution: @unchecked Sendable {
    private let process = Process()
    private let output = Pipe()
    private let error = Pipe()
    private let lock = NSLock()
    private var continuation: CheckedContinuation<ProcessResult, Error>?
    private var stdout: Data?
    private var stderr: Data?
    private var status: Int32?
    private var timedOut = false

    init(
        executable: URL,
        arguments: [String],
        continuation: CheckedContinuation<ProcessResult, Error>
    ) {
        process.executableURL = executable
        process.arguments = arguments
        process.standardOutput = output
        process.standardError = error
        self.continuation = continuation
    }

    func start(timeout: Duration) {
        do {
            try process.run()
        } catch {
            finishImmediately(throwing: error)
            return
        }

        DispatchQueue.global(qos: .utility).async { [self] in
            record(stdout: output.fileHandleForReading.readDataToEndOfFile())
        }
        DispatchQueue.global(qos: .utility).async { [self] in
            record(stderr: error.fileHandleForReading.readDataToEndOfFile())
        }
        DispatchQueue.global(qos: .utility).async { [self] in
            process.waitUntilExit()
            record(status: process.terminationStatus)
        }

        Task.detached { [weak self] in
            try? await Task.sleep(for: timeout)
            self?.expire()
        }
    }

    private func expire() {
        lock.lock()
        guard continuation != nil, status == nil else {
            lock.unlock()
            return
        }
        timedOut = true
        lock.unlock()
        process.terminate()
        Task.detached { [weak self] in
            try? await Task.sleep(for: .milliseconds(250))
            self?.forceTerminateIfNeeded()
        }
    }

    private func forceTerminateIfNeeded() {
        lock.lock()
        guard status == nil else {
            lock.unlock()
            return
        }
        let pid = process.processIdentifier
        lock.unlock()
        _ = Darwin.kill(pid, SIGKILL)
    }

    private func record(stdout: Data) {
        lock.lock()
        self.stdout = stdout
        let completion = takeCompletionIfReady()
        lock.unlock()
        resume(completion)
    }

    private func record(stderr: Data) {
        lock.lock()
        self.stderr = stderr
        let completion = takeCompletionIfReady()
        lock.unlock()
        resume(completion)
    }

    private func record(status: Int32) {
        lock.lock()
        self.status = status
        let completion = takeCompletionIfReady()
        lock.unlock()
        resume(completion)
    }

    private func takeCompletionIfReady() -> Completion? {
        guard let continuation, let stdout, let stderr, let status else { return nil }
        self.continuation = nil
        if timedOut {
            return Completion(continuation: continuation, result: .failure(ProcessRunnerError.timedOut))
        }
        return Completion(
            continuation: continuation,
            result: .success(ProcessResult(status: status, stdout: stdout, stderr: stderr))
        )
    }

    private func finishImmediately(throwing error: Error) {
        lock.lock()
        let continuation = self.continuation
        self.continuation = nil
        lock.unlock()
        continuation?.resume(throwing: error)
    }

    private func resume(_ completion: Completion?) {
        guard let completion else { return }
        completion.continuation.resume(with: completion.result)
    }

    private struct Completion {
        let continuation: CheckedContinuation<ProcessResult, Error>
        let result: Result<ProcessResult, Error>
    }
}
