import Darwin
import Foundation

struct ReconciliationProof: Sendable {
    let appVersion: String
    let embeddedVersion: String
    let daemonVersion: String
    let shellTarget: URL
    let appManaged: Bool
}

struct LegacyInstallSnapshot: Equatable, Sendable {
    let formulaInstalled: Bool
    let cargoBinary: URL?
    let shellLink: ShellLinkState
    let recognizedPaths: Set<URL>
    let unknownPaths: Set<URL>
}

enum LegacyInstallError: Error, Equatable {
    case unrelatedShellCommand(URL)
    case formulaStillInstalled
    case brewUnavailable
    case brewFailed(String)
    case fileOperation(String)
}

protocol LegacyInstallMigrating: Sendable {
    func inspect(canonical: VerifiedAppInstallation) async throws -> LegacyInstallSnapshot
    func removeRecognizedFormula(_ snapshot: LegacyInstallSnapshot) async throws
    func repairShellLink(to executable: URL) async throws -> ShellLinkState
    func removeVerifiedCargoBinary(
        _ snapshot: LegacyInstallSnapshot,
        proof: ReconciliationProof
    ) async throws
}

struct LegacyInstallMigrator: LegacyInstallMigrating {
    private let homeURL: URL
    private let brewURLs: [URL]
    private let runner: any ProcessRunning

    init(
        homeURL: URL = FileManager.default.homeDirectoryForCurrentUser,
        brewURLs: [URL] = [
            URL(fileURLWithPath: "/opt/homebrew/bin/brew"),
            URL(fileURLWithPath: "/usr/local/bin/brew"),
        ],
        runner: any ProcessRunning = ProcessRunner()
    ) {
        self.homeURL = homeURL.standardizedFileURL
        self.brewURLs = brewURLs.map(\.standardizedFileURL)
        self.runner = runner
    }

    func inspect(canonical: VerifiedAppInstallation) async throws -> LegacyInstallSnapshot {
        let shellURL = homeURL.appending(path: ".local/bin/plug")
        let cargoURL = homeURL.appending(path: ".cargo/bin/plug")
        let shellState = inspectShellLink(at: shellURL, canonical: canonical.executableURL)
        let cargoRecognized = await isPlugBinary(at: cargoURL)
        let formulaInstalled = try await installedBrew() != nil

        var recognizedPaths = Set<URL>()
        var unknownPaths = Set<URL>()
        if cargoRecognized {
            recognizedPaths.insert(cargoURL.standardizedFileURL)
        } else if pathExists(cargoURL) {
            unknownPaths.insert(cargoURL.standardizedFileURL)
        }
        if case .unrelated(let url) = shellState {
            unknownPaths.insert(url.standardizedFileURL)
        }
        if formulaInstalled {
            for prefix in ["/opt/homebrew", "/usr/local"] {
                let path = URL(fileURLWithPath: prefix).appending(path: "opt/plug/bin/plug")
                if pathExists(path) { recognizedPaths.insert(path.standardizedFileURL) }
            }
        }

        return LegacyInstallSnapshot(
            formulaInstalled: formulaInstalled,
            cargoBinary: cargoRecognized ? cargoURL.standardizedFileURL : nil,
            shellLink: shellState,
            recognizedPaths: recognizedPaths,
            unknownPaths: unknownPaths
        )
    }

    func removeRecognizedFormula(_ snapshot: LegacyInstallSnapshot) async throws {
        guard snapshot.formulaInstalled else { return }
        guard let brew = try await installedBrew() else {
            throw LegacyInstallError.brewUnavailable
        }
        let result = try await runner.run(
            executable: brew,
            arguments: ["uninstall", "cyberpapiii/tap/plug"],
            timeout: .seconds(60)
        )
        guard result.status == 0 else {
            let detail = String(decoding: result.stderr, as: UTF8.self)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            throw LegacyInstallError.brewFailed(detail)
        }
    }

    func repairShellLink(to executable: URL) async throws -> ShellLinkState {
        let shellURL = homeURL.appending(path: ".local/bin/plug")
        if try await installedBrew() != nil {
            throw LegacyInstallError.formulaStillInstalled
        }
        if case .unrelated = inspectShellLink(at: shellURL, canonical: executable) {
            throw LegacyInstallError.unrelatedShellCommand(shellURL)
        }
        if case .canonical = inspectShellLink(at: shellURL, canonical: executable) {
            return .canonical(executable.standardizedFileURL)
        }

        let parent = shellURL.deletingLastPathComponent()
        do {
            try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: true)
            let temporary = parent.appending(path: ".plug.\(UUID().uuidString).tmp")
            try FileManager.default.createSymbolicLink(
                atPath: temporary.path,
                withDestinationPath: executable.standardizedFileURL.path
            )
            guard Darwin.rename(temporary.path, shellURL.path) == 0 else {
                let message = String(cString: strerror(errno))
                try? FileManager.default.removeItem(at: temporary)
                throw LegacyInstallError.fileOperation(message)
            }
        } catch let error as LegacyInstallError {
            throw error
        } catch {
            throw LegacyInstallError.fileOperation(error.localizedDescription)
        }
        return .canonical(executable.standardizedFileURL)
    }

    func removeVerifiedCargoBinary(
        _ snapshot: LegacyInstallSnapshot,
        proof: ReconciliationProof
    ) async throws {
        guard proof.appManaged,
              proof.appVersion == proof.embeddedVersion,
              proof.embeddedVersion == proof.daemonVersion,
              snapshot.shellLink == .canonical(proof.shellTarget.standardizedFileURL),
              let cargo = snapshot.cargoBinary,
              snapshot.recognizedPaths.contains(cargo.standardizedFileURL),
              cargo.standardizedFileURL == homeURL.appending(path: ".cargo/bin/plug").standardizedFileURL
        else { return }
        guard await isPlugBinary(at: cargo) else { return }
        do {
            try FileManager.default.removeItem(at: cargo)
        } catch {
            throw LegacyInstallError.fileOperation(error.localizedDescription)
        }
    }

    private func installedBrew() async throws -> URL? {
        for brew in brewURLs where pathExists(brew) {
            let result = try await runner.run(
                executable: brew,
                arguments: ["list", "--versions", "cyberpapiii/tap/plug"],
                timeout: .seconds(10)
            )
            if result.status == 0, !result.stdout.isEmpty { return brew }
        }
        return nil
    }

    private func isPlugBinary(at url: URL) async -> Bool {
        guard pathExists(url) else { return false }
        guard let result = try? await runner.run(
            executable: url,
            arguments: ["--version"],
            timeout: .seconds(3)
        ) else { return false }
        let output = String(decoding: result.stdout, as: UTF8.self)
        return result.status == 0 && output.hasPrefix("plug ")
    }

    private func inspectShellLink(at link: URL, canonical: URL) -> ShellLinkState {
        guard pathExistsWithoutFollowing(link) else { return .absent }
        guard let destination = try? FileManager.default.destinationOfSymbolicLink(atPath: link.path) else {
            return .unrelated(link.standardizedFileURL)
        }
        let target = resolvedSymlinkDestination(destination, from: link)
        if target == canonical.standardizedFileURL { return .canonical(target) }
        if !pathExists(target) || isRecognizedLegacyPath(target) { return .repairable(target) }
        return .unrelated(target)
    }

    private func resolvedSymlinkDestination(_ destination: String, from link: URL) -> URL {
        if destination.hasPrefix("/") {
            return URL(fileURLWithPath: destination).standardizedFileURL
        }
        return link.deletingLastPathComponent().appending(path: destination).standardizedFileURL
    }

    private func isRecognizedLegacyPath(_ url: URL) -> Bool {
        let path = url.standardizedFileURL.path
        return path == homeURL.appending(path: ".cargo/bin/plug").standardizedFileURL.path
            || path == "/opt/homebrew/opt/plug/bin/plug"
            || path == "/usr/local/opt/plug/bin/plug"
    }

    private func pathExists(_ url: URL) -> Bool {
        FileManager.default.fileExists(atPath: url.path)
    }

    private func pathExistsWithoutFollowing(_ url: URL) -> Bool {
        var information = stat()
        return url.path.withCString { Darwin.lstat($0, &information) } == 0
    }
}
