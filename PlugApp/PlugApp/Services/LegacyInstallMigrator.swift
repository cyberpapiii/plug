import Darwin
import Foundation
import CryptoKit
import Security

struct ReconciliationProof: Sendable {
    let appVersion: String
    let embeddedVersion: String
    let daemonVersion: String
    let shellTarget: URL
    let daemonExecutable: URL?
    let appManaged: Bool
}

struct LegacyBinaryIdentity: Equatable, Sendable {
    let identifier: String
    let teamID: String
    let sha256: String
}

struct LegacyInstallSnapshot: Equatable, Sendable {
    let formulaInstalled: Bool
    let cargoBinary: URL?
    let cargoBinaryIdentity: LegacyBinaryIdentity?
    let shellLink: ShellLinkState
    let recognizedPaths: Set<URL>
    let unknownPaths: Set<URL>

    init(
        formulaInstalled: Bool,
        cargoBinary: URL?,
        cargoBinaryIdentity: LegacyBinaryIdentity? = nil,
        shellLink: ShellLinkState,
        recognizedPaths: Set<URL>,
        unknownPaths: Set<URL>
    ) {
        self.formulaInstalled = formulaInstalled
        self.cargoBinary = cargoBinary
        self.cargoBinaryIdentity = cargoBinaryIdentity
        self.shellLink = shellLink
        self.recognizedPaths = recognizedPaths
        self.unknownPaths = unknownPaths
    }
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
    private let identityReader: @Sendable (URL) -> LegacyBinaryIdentity?

    init(
        homeURL: URL = FileManager.default.homeDirectoryForCurrentUser,
        brewURLs: [URL] = [
            URL(fileURLWithPath: "/opt/homebrew/bin/brew"),
            URL(fileURLWithPath: "/usr/local/bin/brew"),
        ],
        runner: any ProcessRunning = ProcessRunner(),
        identityReader: @escaping @Sendable (URL) -> LegacyBinaryIdentity? = LegacyInstallMigrator.readLegacyBinaryIdentity
    ) {
        self.homeURL = homeURL.standardizedFileURL
        self.brewURLs = brewURLs.map(\.standardizedFileURL)
        self.runner = runner
        self.identityReader = identityReader
    }

    func inspect(canonical: VerifiedAppInstallation) async throws -> LegacyInstallSnapshot {
        let shellURL = homeURL.appending(path: ".local/bin/plug")
        let cargoURL = homeURL.appending(path: ".cargo/bin/plug")
        let shellState = inspectShellLink(at: shellURL, canonical: canonical.executableURL)
        let cargoIdentity = identityReader(cargoURL)
        let formulaInstalled = try await installedBrew() != nil

        var recognizedPaths = Set<URL>()
        var unknownPaths = Set<URL>()
        switch shellState {
        case .canonical, .repairable:
            // The launchd job records the stable shell-link path, not its
            // destination. Preserve that proven legacy location so daemon
            // adoption can classify and replace it safely.
            recognizedPaths.insert(shellURL.standardizedFileURL)
        case let .unrelated(url):
            unknownPaths.insert(url.standardizedFileURL)
        case .absent:
            break
        }
        if cargoIdentity != nil {
            recognizedPaths.insert(cargoURL.standardizedFileURL)
        } else if pathExists(cargoURL) {
            unknownPaths.insert(cargoURL.standardizedFileURL)
        }
        if formulaInstalled {
            for prefix in ["/opt/homebrew", "/usr/local"] {
                let path = URL(fileURLWithPath: prefix).appending(path: "opt/plug/bin/plug")
                if pathExists(path) { recognizedPaths.insert(path.standardizedFileURL) }
            }
        }

        return LegacyInstallSnapshot(
            formulaInstalled: formulaInstalled,
            cargoBinary: cargoIdentity == nil ? nil : cargoURL.standardizedFileURL,
            cargoBinaryIdentity: cargoIdentity,
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
              proof.daemonExecutable.map({ resolved($0) == resolved(proof.shellTarget) }) == true,
              let cargo = snapshot.cargoBinary,
              let expectedIdentity = snapshot.cargoBinaryIdentity,
              snapshot.recognizedPaths.contains(cargo.standardizedFileURL),
              cargo.standardizedFileURL == homeURL.appending(path: ".cargo/bin/plug").standardizedFileURL
        else { return }
        guard identityReader(cargo) == expectedIdentity else { return }
        do {
            try removeRegularFileWithoutFollowing(at: cargo)
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

    private func removeRegularFileWithoutFollowing(at url: URL) throws {
        var information = stat()
        guard url.path.withCString({ Darwin.lstat($0, &information) }) == 0 else {
            throw POSIXError(.init(rawValue: errno) ?? .ENOENT)
        }
        guard (information.st_mode & S_IFMT) == S_IFREG else {
            throw POSIXError(.EFTYPE)
        }
        guard Darwin.unlink(url.path) == 0 else {
            throw POSIXError(.init(rawValue: errno) ?? .EIO)
        }
    }

    private func resolved(_ url: URL) -> URL {
        url.standardizedFileURL.resolvingSymlinksInPath().standardizedFileURL
    }

    private static func readLegacyBinaryIdentity(at url: URL) -> LegacyBinaryIdentity? {
        guard isRegularFileWithoutFollowing(at: url),
              FileManager.default.isExecutableFile(atPath: url.path)
        else { return nil }

        var staticCode: SecStaticCode?
        guard SecStaticCodeCreateWithPath(url as CFURL, [], &staticCode) == errSecSuccess,
              let staticCode
        else { return nil }

        let requirementText = "anchor apple generic and identifier \"plug\" and certificate leaf[subject.OU] = \"\(AppInstallationInspector.teamID)\""
        var requirement: SecRequirement?
        guard SecRequirementCreateWithString(requirementText as CFString, [], &requirement) == errSecSuccess,
              let requirement,
              SecStaticCodeCheckValidity(staticCode, [], requirement) == errSecSuccess
        else { return nil }

        var signingInformation: CFDictionary?
        guard SecCodeCopySigningInformation(staticCode, SecCSFlags(rawValue: kSecCSSigningInformation), &signingInformation) == errSecSuccess,
              let information = signingInformation as? [String: Any],
              let identifier = information[kSecCodeInfoIdentifier as String] as? String,
              let teamID = information[kSecCodeInfoTeamIdentifier as String] as? String,
              teamID == AppInstallationInspector.teamID,
              identifier == "plug",
              let data = try? Data(contentsOf: url)
        else { return nil }

        return LegacyBinaryIdentity(
            identifier: identifier,
            teamID: teamID,
            sha256: SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        )
    }

    private static func isRegularFileWithoutFollowing(at url: URL) -> Bool {
        var information = stat()
        guard url.path.withCString({ Darwin.lstat($0, &information) }) == 0 else { return false }
        return (information.st_mode & S_IFMT) == S_IFREG
    }
}
