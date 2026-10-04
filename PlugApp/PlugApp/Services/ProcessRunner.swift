import Darwin
import Foundation

struct ProcessResult: Sendable {
    let status: Int32
    let stdout: Data
    let stderr: Data

    /// Standard error as trimmed text, for the failure messages every caller
    /// builds the same way.
    var stderrText: String {
        String(decoding: stderr, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

enum ProcessRunnerError: Error, Equatable, LocalizedError {
    case timedOut

    /// Every caller surfaces `localizedDescription` straight into the UI, and
    /// the synthesized one for a bare enum reads as "error 0".
    var errorDescription: String? {
        switch self {
        case .timedOut: "Plug took too long to answer. Try again."
        }
    }
}

protocol ProcessRunning: Sendable {
    func run(executable: URL, arguments: [String], timeout: Duration) async throws -> ProcessResult
}

/// Cancelling the calling task stops the child's whole process group, the
/// same way a timeout does, and the call throws `CancellationError`.
struct ProcessRunner: ProcessRunning {
    func run(
        executable: URL,
        arguments: [String],
        timeout: Duration
    ) async throws -> ProcessResult {
        let execution = ProcessExecution(executable: executable, arguments: arguments)
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                execution.start(timeout: timeout, continuation: continuation)
            }
        } onCancel: {
            execution.cancel()
        }
    }
}

private final class ProcessExecution: @unchecked Sendable {
    private let executable: URL
    private let arguments: [String]
    private let output = Pipe()
    private let error = Pipe()
    private let lock = NSLock()
    private var processGroup: pid_t?
    private var continuation: CheckedContinuation<ProcessResult, Error>?
    private var stdout: Data?
    private var stderr: Data?
    private var status: Int32?
    private var timedOut = false
    private var cancelled = false

    init(executable: URL, arguments: [String]) {
        self.executable = executable
        self.arguments = arguments
    }

    func start(timeout: Duration, continuation: CheckedContinuation<ProcessResult, Error>) {
        lock.lock()
        self.continuation = continuation
        let cancelledBeforeStart = cancelled
        lock.unlock()
        guard !cancelledBeforeStart else {
            finishImmediately(throwing: CancellationError())
            return
        }

        let group: pid_t
        do {
            group = try spawnInIsolatedProcessGroup()
        } catch {
            finishImmediately(throwing: error)
            return
        }
        lock.lock()
        processGroup = group
        // A cancel that landed during the spawn found no group to stop.
        let cancelledDuringSpawn = cancelled
        lock.unlock()

        DispatchQueue.global(qos: .utility).async { [self] in
            record(stdout: output.fileHandleForReading.readDataToEndOfFile())
        }
        DispatchQueue.global(qos: .utility).async { [self] in
            record(stderr: error.fileHandleForReading.readDataToEndOfFile())
        }
        DispatchQueue.global(qos: .utility).async { [self] in
            record(status: waitForExit(of: group))
        }

        if cancelledDuringSpawn { terminate(group) }
        Task.detached { [weak self] in
            try? await Task.sleep(for: timeout)
            self?.expire()
        }
    }

    func cancel() {
        lock.lock()
        cancelled = true
        let group = continuation != nil ? processGroup : nil
        lock.unlock()
        if let group { terminate(group) }
    }

    private func expire() {
        lock.lock()
        guard continuation != nil, let processGroup else {
            lock.unlock()
            return
        }
        timedOut = true
        lock.unlock()
        terminate(processGroup)
    }

    private func terminate(_ processGroup: pid_t) {
        _ = Darwin.kill(-processGroup, SIGTERM)
        Task.detached { [weak self] in
            try? await Task.sleep(for: .milliseconds(250))
            self?.forceTerminateIfNeeded()
        }
    }

    private func forceTerminateIfNeeded() {
        lock.lock()
        guard continuation != nil, let processGroup else {
            lock.unlock()
            return
        }
        lock.unlock()
        _ = Darwin.kill(-processGroup, SIGKILL)
    }

    private func spawnInIsolatedProcessGroup() throws -> pid_t {
        var fileActions: posix_spawn_file_actions_t?
        var attributes: posix_spawnattr_t?
        try check(posix_spawn_file_actions_init(&fileActions))
        defer { posix_spawn_file_actions_destroy(&fileActions) }
        try check(posix_spawnattr_init(&attributes))
        defer {
            posix_spawnattr_destroy(&attributes)
        }

        let outputRead = output.fileHandleForReading.fileDescriptor
        let outputWrite = output.fileHandleForWriting.fileDescriptor
        let errorRead = error.fileHandleForReading.fileDescriptor
        let errorWrite = error.fileHandleForWriting.fileDescriptor

        try check(posix_spawn_file_actions_adddup2(&fileActions, outputWrite, STDOUT_FILENO))
        try check(posix_spawn_file_actions_adddup2(&fileActions, errorWrite, STDERR_FILENO))
        try check(posix_spawn_file_actions_addclose(&fileActions, outputRead))
        try check(posix_spawn_file_actions_addclose(&fileActions, errorRead))
        try check(posix_spawn_file_actions_addclose(&fileActions, outputWrite))
        try check(posix_spawn_file_actions_addclose(&fileActions, errorWrite))
        try check(posix_spawnattr_setflags(&attributes, Int16(POSIX_SPAWN_SETPGROUP)))
        try check(posix_spawnattr_setpgroup(&attributes, 0))

        var processID = pid_t()
        let values = [executable.path] + arguments
        var arguments = values.map { strdup($0) }
        arguments.append(nil)
        defer { arguments.compactMap { $0 }.forEach { free($0) } }

        let spawnStatus = arguments.withUnsafeMutableBufferPointer { buffer in
            posix_spawn(
                &processID,
                executable.path,
                &fileActions,
                &attributes,
                buffer.baseAddress!,
                environ
            )
        }
        try check(spawnStatus)

        output.fileHandleForWriting.closeFile()
        error.fileHandleForWriting.closeFile()
        return processID
    }

    private func check(_ status: Int32) throws {
        guard status == 0 else {
            throw POSIXError(POSIXErrorCode(rawValue: status) ?? .EINVAL)
        }
    }

    private func waitForExit(of processID: pid_t) -> Int32 {
        var rawStatus: Int32 = 0
        while Darwin.waitpid(processID, &rawStatus, 0) == -1, errno == EINTR {}
        let signal = rawStatus & 0x7f
        return signal == 0 ? (rawStatus >> 8) & 0xff : 128 + signal
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
        if cancelled {
            return Completion(continuation: continuation, result: .failure(CancellationError()))
        }
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
