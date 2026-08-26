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
            guard let program = record.programURL.map(Self.resolvedPath) else { return false }
            return legacyPaths.contains(program)
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

    private static func parseRecord(label: String, output: String) -> LaunchdJobRecord {
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
              record.parentBundleIdentifier == AppInstallationInspector.bundleIdentifier,
              record.parentBundleVersion == canonical.buildVersion
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

    private static func resolvedPath(_ url: URL) -> URL {
        url.standardizedFileURL.resolvingSymlinksInPath().standardizedFileURL
    }

    private static func errorDetail(_ result: ProcessResult) -> String {
        String(decoding: result.stderr, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
