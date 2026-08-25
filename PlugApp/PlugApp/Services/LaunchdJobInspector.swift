import Darwin
import Foundation

protocol LaunchdJobInspecting: Sendable {
    func daemonJobs(
        canonical: VerifiedAppInstallation,
        recognizedLegacyPaths: Set<URL>
    ) async throws -> DaemonOwnershipState
}

struct LaunchdJobInspector: LaunchdJobInspecting {
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
        let allRecords = try await records()
        let canonicalPath = canonical.executableURL.standardizedFileURL
        let legacyPaths = Set(recognizedLegacyPaths.map(\.standardizedFileURL))
        let relevant = allRecords.filter { record in
            record.label.localizedCaseInsensitiveContains("plug")
                || record.programURL?.lastPathComponent == "plug"
                || record.parentBundleIdentifier == AppInstallationInspector.bundleIdentifier
        }
        guard !relevant.isEmpty else { return .unmanaged }

        let appOwned = relevant.filter {
            $0.programURL?.standardizedFileURL == canonicalPath
                && $0.parentBundleIdentifier == AppInstallationInspector.bundleIdentifier
        }
        if appOwned.count == 1, relevant.count == 1, let record = appOwned.first {
            return record.parentBundleVersion == canonical.buildVersion
                ? .appManagedCurrent(record)
                : .appManagedStale(record)
        }

        let recognized = relevant.filter { record in
            guard let program = record.programURL?.standardizedFileURL else { return false }
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
        guard listed.status == 0 else { return [] }
        let labels = parseLabels(String(decoding: listed.stdout, as: UTF8.self))
        var records: [LaunchdJobRecord] = []
        for label in labels {
            let detail = try await runner.run(
                executable: launchctl,
                arguments: ["print", "gui/\(userID)/\(label)"],
                timeout: .seconds(5)
            )
            guard detail.status == 0 else { continue }
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
        var parentIdentifier: String?
        var parentVersion: String?
        var loaded = false
        for line in output.split(separator: "\n") {
            let text = line.trimmingCharacters(in: .whitespaces)
            if let value = value(after: "program =", in: text) {
                program = URL(fileURLWithPath: value).standardizedFileURL
            } else if let value = value(after: "parent bundle identifier =", in: text) {
                parentIdentifier = value
            } else if let value = value(after: "parent bundle version =", in: text) {
                parentVersion = value
            } else if text == "state = running" {
                loaded = true
            }
        }
        return LaunchdJobRecord(
            label: label,
            programURL: program,
            parentBundleIdentifier: parentIdentifier,
            parentBundleVersion: parentVersion,
            loaded: loaded
        )
    }

    private static func value(after prefix: String, in line: String) -> String? {
        guard line.hasPrefix(prefix) else { return nil }
        return String(line.dropFirst(prefix.count)).trimmingCharacters(in: .whitespaces)
    }
}
