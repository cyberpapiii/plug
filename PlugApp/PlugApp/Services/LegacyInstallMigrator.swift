import Darwin
import Foundation
import CryptoKit
import Security

/// A `cargo install` copy of Plug signed by Plug's team, by its contents, so
/// a file swapped in after inspection is never the one removed.
struct LegacyBinaryIdentity: Equatable, Sendable {
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

enum LegacyInstallError: LocalizedError, AdvisedError, Equatable {
    case unrelatedShellCommand(URL)
    case formulaStillInstalled
    case brewUnavailable
    case brewFailed(String)
    case fileOperation(String)

    var errorDescription: String? {
        switch self {
        case let .unrelatedShellCommand(path):
            "Another file is in the way at \(path.path). Move it, then try again."
        case .formulaStillInstalled:
            "The old Homebrew copy of Plug is still installed. Run brew uninstall \(LegacyPlugProgram.formula), then try again."
        case .brewUnavailable:
            "Plug could not run Homebrew, so the old Homebrew copy of Plug stays."
        case let .brewFailed(detail):
            "Homebrew could not remove the old copy of Plug: \(detail)"
        case let .fileOperation(message):
            message
        }
    }

    var advice: String {
        switch self {
        case let .brewFailed(detail), let .fileOperation(detail): Explain.advice(forOutside: detail)
        default: Explain.fallback
        }
    }
}

protocol LegacyInstallMigrating: Sendable {
    func inspect(canonical: VerifiedAppInstallation) async throws -> LegacyInstallSnapshot
    func removeRecognizedFormula(_ snapshot: LegacyInstallSnapshot) async throws
    func repairShellLink(to executable: URL) async throws -> ShellLinkState
    func removeVerifiedCargoBinary(
        _ snapshot: LegacyInstallSnapshot,
        proof: OwnDaemonProof
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
        let shellURL = homeURL.appending(path: LegacyPlugProgram.shellLink)
        let cargoURL = homeURL.appending(path: LegacyPlugProgram.cargoBinary)
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
            for link in LegacyPlugProgram.formulaLinks.map({ URL(fileURLWithPath: $0) }) where pathExists(link) {
                recognizedPaths.insert(link.standardizedFileURL)
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
            arguments: ["uninstall", LegacyPlugProgram.formula],
            timeout: .seconds(60)
        )
        guard result.status == 0 else {
            let detail = result.stderrText
            throw LegacyInstallError.brewFailed(detail)
        }
    }

    func repairShellLink(to executable: URL) async throws -> ShellLinkState {
        let shellURL = homeURL.appending(path: LegacyPlugProgram.shellLink)
        if try await installedBrew() != nil {
            throw LegacyInstallError.formulaStillInstalled
        }
        switch inspectShellLink(at: shellURL, canonical: executable) {
        case .unrelated:
            throw LegacyInstallError.unrelatedShellCommand(shellURL)
        case .canonical:
            return .canonical(executable.standardizedFileURL)
        case .absent, .repairable:
            break
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

    /// Only with the daemon proved to be the app's own, so the command a
    /// person types never stops working.
    func removeVerifiedCargoBinary(
        _ snapshot: LegacyInstallSnapshot,
        proof: OwnDaemonProof
    ) async throws {
        guard let cargo = snapshot.cargoBinary,
              let expectedIdentity = snapshot.cargoBinaryIdentity,
              snapshot.recognizedPaths.contains(cargo.standardizedFileURL),
              cargo.standardizedFileURL == homeURL.appending(path: LegacyPlugProgram.cargoBinary).standardizedFileURL
        else { return }
        guard identityReader(cargo) == expectedIdentity else { return }
        do {
            try removeRegularFileWithoutFollowing(at: cargo)
        } catch {
            throw LegacyInstallError.fileOperation(error.localizedDescription)
        }
    }

    private func installedBrew() async throws -> URL? {
        for brew in brewURLs where pathExists(brew) && formulaMayBeInstalled(beside: brew) {
            let result = try await runner.run(
                executable: brew,
                arguments: ["list", "--versions", LegacyPlugProgram.formula],
                timeout: .seconds(10)
            )
            if result.status == 0, !result.stdout.isEmpty { return brew }
        }
        return nil
    }

    /// Asking brew costs about a second a call, more at login, and a launch
    /// used to ask two or three times. An installed formula always leaves
    /// `opt/plug` or `Cellar/plug` under its prefix (`/opt/homebrew` or
    /// `/usr/local`, the directory above brew's `bin`), so with neither there
    /// is no formula to ask about. With either, brew still has the final word.
    private func formulaMayBeInstalled(beside brew: URL) -> Bool {
        let prefix = brew.deletingLastPathComponent().deletingLastPathComponent()
        return ["opt/plug", "Cellar/plug"].contains {
            pathExistsWithoutFollowing(prefix.appending(path: $0))
        }
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
        return path == homeURL.appending(path: LegacyPlugProgram.cargoBinary).standardizedFileURL.path
            || LegacyPlugProgram.formulaLinks.contains(path)
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

    private static func readLegacyBinaryIdentity(at url: URL) -> LegacyBinaryIdentity? {
        guard isRegularFileWithoutFollowing(at: url),
              FileManager.default.isExecutableFile(atPath: url.path)
        else { return nil }

        var staticCode: SecStaticCode?
        guard SecStaticCodeCreateWithPath(url as CFURL, [], &staticCode) == errSecSuccess,
              let staticCode
        else { return nil }

        let requirementText = AppInstallationInspector.signingRequirement(identifier: "plug")
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
            sha256: SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        )
    }

    private static func isRegularFileWithoutFollowing(at url: URL) -> Bool {
        var information = stat()
        guard url.path.withCString({ Darwin.lstat($0, &information) }) == 0 else { return false }
        return (information.st_mode & S_IFMT) == S_IFREG
    }
}
