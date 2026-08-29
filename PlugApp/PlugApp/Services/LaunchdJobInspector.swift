import Darwin
import Foundation

protocol LaunchdJobInspecting: Sendable {
    func daemonJobs(
        canonical: VerifiedAppInstallation,
        recognizedLegacyPaths: Set<URL>
    ) async throws -> DaemonOwnershipState
}

enum LaunchdJobInspectionError: Error, Equatable {
    case listFailed(status: Int32, detail: String)
    case printFailed(label: String, status: Int32, detail: String)
}

struct LaunchdJobInspector: LaunchdJobInspecting {
    private static let appProgramIdentifier = "Contents/Resources/plug"
    private static let appArguments = [appProgramIdentifier, "serve", "--daemon"]

    private let records: @Sendable () async throws -> [LaunchdJobRecord]

    init(runner: any ProcessRunning = ProcessRunner(), userID: uid_t = getuid()) {
        records = {
            try await Self.loadRecords(runner: runner, userID: userID)
        }
    }

    init(records: @escaping @Sendable () async throws -> [LaunchdJobRecord]) {
        self.records = records
    }

    func daemonJobs(
        canonical: VerifiedAppInstallation,
        recognizedLegacyPaths: Set<URL>
    ) async throws -> DaemonOwnershipState {
        let allRecords = try await records().map {
            Self.resolveAppManagedProgram(in: $0, canonical: canonical)
        }
        let canonicalPath = Self.resolvedPath(canonical.executableURL)
        let legacyPaths = Set(recognizedLegacyPaths.map(Self.resolvedPath))
        let relevant = allRecords.filter { record in
            record.label == "com.plug.daemon"
                || record.programURL?.lastPathComponent == "plug"
                || record.programIdentifier == Self.appProgramIdentifier
                || record.arguments.first == Self.appProgramIdentifier
        }
        guard !relevant.isEmpty else { return .unmanaged }

        let appOwned = relevant.filter {
            $0.programURL.map(Self.resolvedPath) == canonicalPath
                && $0.parentBundleIdentifier == AppInstallationInspector.bundleIdentifier
        }
        if appOwned.count == 1, relevant.count == 1, let record = appOwned.first {
            return record.parentBundleVersion == canonical.buildVersion
                ? .appManagedCurrent(record)
                : .appManagedStale(record)
        }

        let recognized = relevant.filter { record in
            guard let programURL = record.programURL else { return false }
            let program = Self.resolvedPath(programURL)
            return legacyPaths.contains(program)
                || LegacyPlugProgram.isRecognized(programURL)
                || LegacyPlugProgram.isRecognized(program)
        }
        if recognized.count == relevant.count {
            return .recognizedLegacy(recognized)
        }
        return .unknown(relevant)
    }

    private static func loadRecords(
        runner: any ProcessRunning,
        userID: uid_t
    ) async throws -> [LaunchdJobRecord] {
        let launchctl = URL(fileURLWithPath: "/bin/launchctl")
        let listed = try await runner.run(
            executable: launchctl,
            arguments: ["list"],
            timeout: .seconds(10)
        )
        guard listed.status == 0 else {
            throw LaunchdJobInspectionError.listFailed(
                status: listed.status,
                detail: errorDetail(listed)
            )
        }
        let labels = parseLabels(String(decoding: listed.stdout, as: UTF8.self))
        var records: [LaunchdJobRecord] = []
        for label in labels {
            let detail = try await runner.run(
                executable: launchctl,
                arguments: ["print", "gui/\(userID)/\(label)"],
                timeout: .seconds(5)
            )
            if detail.status != 0, label != "com.plug.daemon" {
                // `launchctl list` includes transient jobs that can disappear
                // before `print`. Their absence proves there is nothing left
                // to inspect or mutate. Plug's exact service remains
                // fail-closed below.
                continue
            }
            guard detail.status == 0 else {
                throw LaunchdJobInspectionError.printFailed(
                    label: label,
                    status: detail.status,
                    detail: errorDetail(detail)
                )
            }
            records.append(parseRecord(label: label, output: String(decoding: detail.stdout, as: UTF8.self)))
        }
        return records
    }

    private static func parseLabels(_ output: String) -> [String] {
        output.split(separator: "\n").compactMap { line in
            let fields = line.split(whereSeparator: \.isWhitespace)
            guard fields.count >= 3, fields.last != "Label" else { return nil }
            return fields.last.map(String.init)
        }
    }

    static func parseRecord(label: String, output: String) -> LaunchdJobRecord {
        var program: URL?
        var programIdentifier: String?
        var parentIdentifier: String?
        var parentVersion: String?
        var arguments: [String] = []
        var parsingArguments = false
        var loaded = false
        for line in output.split(separator: "\n") {
            let text = line.trimmingCharacters(in: .whitespaces)
            if parsingArguments {
                if text == "}" {
                    parsingArguments = false
                } else if !text.isEmpty {
                    arguments.append(text)
                }
            } else if text == "arguments = {" {
                parsingArguments = true
            } else if let value = value(after: "program identifier =", in: text) {
                programIdentifier = value.components(separatedBy: " (mode:").first
            } else if let value = value(after: "program =", in: text) {
                program = URL(fileURLWithPath: value).standardizedFileURL
            } else if let value = value(after: "parent bundle identifier =", in: text) {
                parentIdentifier = value
            } else if let value = value(after: "parent bundle version =", in: text) {
                parentVersion = value
            } else if text == "state = running" {
                loaded = true
            }
        }
        if parsingArguments {
            arguments = []
        }
        return LaunchdJobRecord(
            label: label,
            programURL: program,
            parentBundleIdentifier: parentIdentifier,
            parentBundleVersion: parentVersion,
            loaded: loaded,
            programIdentifier: programIdentifier,
            arguments: arguments
        )
    }

    private static func resolveAppManagedProgram(
        in record: LaunchdJobRecord,
        canonical: VerifiedAppInstallation
    ) -> LaunchdJobRecord {
        guard record.programURL == nil,
              record.programIdentifier == appProgramIdentifier,
              record.arguments == appArguments,
              record.parentBundleIdentifier == AppInstallationInspector.bundleIdentifier
        else {
            return record
        }

        let candidate = canonical.bundleURL
            .appending(path: appProgramIdentifier)
            .standardizedFileURL
        guard resolvedPath(candidate) == resolvedPath(canonical.executableURL) else {
            return record
        }

        return LaunchdJobRecord(
            label: record.label,
            programURL: candidate,
            parentBundleIdentifier: record.parentBundleIdentifier,
            parentBundleVersion: record.parentBundleVersion,
            loaded: record.loaded,
            programIdentifier: record.programIdentifier,
            arguments: record.arguments
        )
    }

    private static func value(after prefix: String, in line: String) -> String? {
        guard line.hasPrefix(prefix) else { return nil }
        return String(line.dropFirst(prefix.count)).trimmingCharacters(in: .whitespaces)
    }

    static func resolvedPath(_ url: URL) -> URL {
        url.resolvedStandardized
    }

    private static func errorDetail(_ result: ProcessResult) -> String {
        result.stderrText
    }
}

/// Re-reads one launchd job in the moment before it is torn down.
///
/// `launchctl bootout` addresses a job by label, and a label is not an
/// identity. Between the inspection that recognized a job and the bootout that
/// removes it, the same label can come to belong to a different program, so
/// the label alone is not evidence that the job about to be removed is the one
/// the inspection authorized removing.
struct LaunchdJobProbe: Sendable {
    enum Outcome: Equatable {
        /// The label still names the program the record described.
        case unchanged
        /// The job is already gone, so there is nothing left to tear down.
        case vanished
        /// The label now names a different program, or one that cannot be read.
        case replaced(URL?)
    }

    private let runner: any ProcessRunning
    private let userID: uid_t

    init(runner: any ProcessRunning = ProcessRunner(), userID: uid_t = getuid()) {
        self.runner = runner
        self.userID = userID
    }

    func verify(_ record: LaunchdJobRecord) async throws -> Outcome {
        guard let expected = record.programURL else {
            throw DaemonServiceError.invalidJobEvidence
        }
        let result = try await runner.run(
            executable: URL(fileURLWithPath: "/bin/launchctl"),
            arguments: ["print", "gui/\(userID)/\(record.label)"],
            timeout: .seconds(5)
        )
        guard result.status == 0 else { return .vanished }
        let current = LaunchdJobInspector.parseRecord(
            label: record.label,
            output: String(decoding: result.stdout, as: UTF8.self)
        )
        // A job that reports no program path cannot be matched against the
        // record, so it is treated as replaced rather than assumed unchanged.
        // Only recognized-legacy jobs are ever booted out, and those always
        // carry a concrete `program`.
        guard let program = current.programURL else { return .replaced(nil) }
        let same = LaunchdJobInspector.resolvedPath(program)
            == LaunchdJobInspector.resolvedPath(expected)
        return same ? .unchanged : .replaced(program)
    }
}
