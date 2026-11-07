import Foundation
import PlugIPC
import Security

struct AppSignatureEvidence: Equatable, Sendable {
    let valid: Bool
    let bundleIdentifier: String
    let teamID: String
}

enum AppInstallationError: LocalizedError, Equatable {
    case invalidSignature
    case wrongBundleIdentifier(String)
    case untrustedTeam(String)
    case missingMetadata
    case missingEmbeddedExecutable(URL)
    case embeddedVersionFailure(String)
    case versionMismatch(app: String, embedded: String)

    var errorDescription: String? {
        switch self {
        case .invalidSignature:
            "This copy of Plug is damaged or was changed. Download Plug again and replace this copy."
        case let .wrongBundleIdentifier(identifier):
            "This app is \(identifier), not Plug. Download Plug again and replace this copy."
        case let .untrustedTeam(team):
            "This copy of Plug was signed by someone else (\(team)). Download Plug again and replace this copy."
        case .missingMetadata:
            "This copy of Plug does not say which version it is. Download Plug again and replace this copy."
        case .missingEmbeddedExecutable:
            "This copy of Plug has no service inside it. Download Plug again and replace this copy."
        case let .embeddedVersionFailure(detail):
            "Plug's service could not say which version it is: \(detail)"
        case let .versionMismatch(app, embedded):
            "This copy of Plug is version \(app), and the service inside it is \(embedded). Download Plug again and replace this copy."
        }
    }
}

protocol AppInstallationInspecting: Sendable {
    func inspectCurrentApp() async throws -> VerifiedAppInstallation
}

struct AppInstallationInspector: AppInstallationInspecting {
    static let bundleIdentifier = PlugPaths.appBundleID
    static let teamID = PlugPaths.teamID

    /// The code-signing requirement a Plug binary meets: Apple-anchored, with
    /// this identifier, signed by Plug's team.
    static func signingRequirement(identifier: String) -> String {
        "anchor apple generic and identifier \"\(identifier)\" and certificate leaf[subject.OU] = \"\(teamID)\""
    }

    private let bundleURL: @Sendable () -> URL
    private let signatureReader: @Sendable (URL) throws -> AppSignatureEvidence
    private let infoReader: @Sendable (URL) throws -> [String: String]
    private let embeddedVersionReader: @Sendable (URL) async throws -> String

    init() {
        bundleURL = { Bundle.main.bundleURL }
        signatureReader = Self.readSignature
        infoReader = Self.readBundleInfo
        embeddedVersionReader = { try await Self.readEmbeddedVersion(at: $0) }
    }

    init(
        bundleURL: @escaping @Sendable () -> URL,
        signatureReader: @escaping @Sendable (URL) throws -> AppSignatureEvidence,
        infoReader: @escaping @Sendable (URL) throws -> [String: String],
        embeddedVersionReader: @escaping @Sendable (URL) async throws -> String
    ) {
        self.bundleURL = bundleURL
        self.signatureReader = signatureReader
        self.infoReader = infoReader
        self.embeddedVersionReader = embeddedVersionReader
    }

    func inspectCurrentApp() async throws -> VerifiedAppInstallation {
        let bundleURL = bundleURL().standardizedFileURL
        let signature = try signatureReader(bundleURL)
        guard signature.valid else { throw AppInstallationError.invalidSignature }
        guard signature.bundleIdentifier == Self.bundleIdentifier else {
            throw AppInstallationError.wrongBundleIdentifier(signature.bundleIdentifier)
        }
        guard signature.teamID == Self.teamID else {
            throw AppInstallationError.untrustedTeam(signature.teamID)
        }

        let info = try infoReader(bundleURL)
        guard let appVersion = info["CFBundleShortVersionString"],
              let buildVersion = info["CFBundleVersion"],
              !appVersion.isEmpty,
              !buildVersion.isEmpty
        else {
            throw AppInstallationError.missingMetadata
        }

        let executableURL = bundleURL.appending(path: PlugPaths.bundledBinary)
        let embeddedVersion = try await embeddedVersionReader(executableURL)
        guard embeddedVersion == appVersion else {
            throw AppInstallationError.versionMismatch(app: appVersion, embedded: embeddedVersion)
        }

        return VerifiedAppInstallation(
            bundleURL: bundleURL,
            executableURL: executableURL,
            appVersion: appVersion,
            buildVersion: buildVersion
        )
    }

    static func readSignature(at bundleURL: URL) throws -> AppSignatureEvidence {
        var staticCode: SecStaticCode?
        guard SecStaticCodeCreateWithPath(bundleURL as CFURL, [], &staticCode) == errSecSuccess,
              let staticCode
        else {
            throw AppInstallationError.invalidSignature
        }

        let requirementText = signingRequirement(identifier: bundleIdentifier)
        var requirement: SecRequirement?
        guard SecRequirementCreateWithString(requirementText as CFString, [], &requirement) == errSecSuccess,
              let requirement
        else {
            throw AppInstallationError.invalidSignature
        }

        var signingInformation: CFDictionary?
        guard SecCodeCopySigningInformation(staticCode, SecCSFlags(rawValue: kSecCSSigningInformation), &signingInformation) == errSecSuccess,
              let information = signingInformation as? [String: Any]
        else {
            throw AppInstallationError.invalidSignature
        }

        return AppSignatureEvidence(
            valid: SecStaticCodeCheckValidity(staticCode, [], requirement) == errSecSuccess,
            bundleIdentifier: information[kSecCodeInfoIdentifier as String] as? String ?? "",
            teamID: information[kSecCodeInfoTeamIdentifier as String] as? String ?? ""
        )
    }

    static func readBundleInfo(at bundleURL: URL) throws -> [String: String] {
        guard let bundle = Bundle(url: bundleURL) else {
            throw AppInstallationError.missingMetadata
        }
        return [
            "CFBundleShortVersionString": bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "",
            "CFBundleVersion": bundle.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "",
        ]
    }

    /// The version `plug --version` prints, its last word.
    static func readEmbeddedVersion(
        at executableURL: URL,
        runner: any ProcessRunning = ProcessRunner()
    ) async throws -> String {
        guard FileManager.default.isExecutableFile(atPath: executableURL.path) else {
            throw AppInstallationError.missingEmbeddedExecutable(executableURL)
        }
        let result = try await runner.run(
            executable: executableURL,
            arguments: ["--version"],
            // Right after login the first run of a fresh binary can wait on
            // disk and on Gatekeeper for well over three seconds.
            timeout: .seconds(15)
        )
        let output = String(decoding: result.stdout, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard result.status == 0, let version = output.split(whereSeparator: \.isWhitespace).last else {
            let detail = result.stderrText
            throw AppInstallationError.embeddedVersionFailure(detail)
        }
        return String(version)
    }
}
