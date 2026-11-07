import Foundation
import XCTest
@testable import Plug

final class AppInstallationInspectorTests: XCTestCase {
    func testAcceptsVerifiedAppAtAnyInstallLocation() async throws {
        let locations = [
            URL(fileURLWithPath: "/Applications/Plug.app"),
            FileManager.default.homeDirectoryForCurrentUser.appending(path: "Applications/Plug.app"),
            URL(fileURLWithPath: "/Volumes/Tools/Plug.app"),
        ]

        for location in locations {
            let inspector = fixture(bundleURL: location)
            let installation = try await inspector.inspectCurrentApp()

            XCTAssertEqual(installation.bundleURL, location)
            XCTAssertEqual(installation.executableURL, location.appending(path: "Contents/Resources/plug"))
            XCTAssertEqual(installation.appVersion, "0.7.0")
            XCTAssertEqual(installation.buildVersion, "20")
        }
    }

    func testRejectsWrongDeveloperTeam() async {
        let inspector = fixture(teamID: "ATTACKER123")

        await assertThrows(.untrustedTeam("ATTACKER123")) {
            _ = try await inspector.inspectCurrentApp()
        }
    }

    func testRejectsASignatureThatDoesNotHold() async {
        await assertThrows(.invalidSignature) {
            _ = try await fixture(valid: false).inspectCurrentApp()
        }
    }

    func testRejectsAnotherAppSignedByTheSameTeam() async {
        await assertThrows(.wrongBundleIdentifier("com.example.other")) {
            _ = try await fixture(bundleIdentifier: "com.example.other").inspectCurrentApp()
        }
    }

    func testRejectsABundleWithoutItsVersions() async {
        for info in [[:], ["CFBundleShortVersionString": "0.7.0"], ["CFBundleShortVersionString": "", "CFBundleVersion": "20"]] {
            await assertThrows(.missingMetadata) {
                _ = try await fixture(info: info).inspectCurrentApp()
            }
        }
    }

    /// The reconcile panel and a failed press show these as they are.
    func testEveryFailureReadsAsASentence() {
        let errors: [AppInstallationError] = [
            .invalidSignature, .wrongBundleIdentifier("x"), .untrustedTeam("y"), .missingMetadata,
            .missingEmbeddedExecutable(URL(fileURLWithPath: "/x")), .embeddedVersionFailure("z"),
            .versionMismatch(app: "1", embedded: "2"),
        ]
        for error in errors {
            XCTAssertEqual(error.localizedDescription, error.errorDescription, "\(error)")
            XCTAssertTrue(error.localizedDescription.hasPrefix("This") || error.localizedDescription.hasPrefix("Plug"))
        }
    }

    func testRejectsAppAndEmbeddedBinaryVersionMismatch() async {
        let inspector = fixture(embeddedVersion: "0.6.4")

        await assertThrows(.versionMismatch(app: "0.7.0", embedded: "0.6.4")) {
            _ = try await inspector.inspectCurrentApp()
        }
    }

    // MARK: The real readers

    private func temporaryDirectory() throws -> URL {
        let directory = FileManager.default.temporaryDirectory
            .appending(path: "plug-inspector-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: directory) }
        return directory
    }

    private func script(_ body: String, in directory: URL) throws -> URL {
        let url = directory.appending(path: "plug")
        try "#!/bin/sh\n\(body)\n".write(to: url, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
        return url
    }

    func testTheEmbeddedVersionIsTheLastWordOfVersionOutput() async throws {
        let binary = try script("echo 'plug 1.2.3'", in: try temporaryDirectory())
        let version = try await AppInstallationInspector.readEmbeddedVersion(at: binary)
        XCTAssertEqual(version, "1.2.3")
    }

    func testAFailingVersionRunIsAnError() async throws {
        let binary = try script("echo broken >&2; exit 3", in: try temporaryDirectory())
        await assertThrows(.embeddedVersionFailure("broken")) {
            _ = try await AppInstallationInspector.readEmbeddedVersion(at: binary)
        }
    }

    func testAMissingEmbeddedBinaryIsNamed() async throws {
        let missing = try temporaryDirectory().appending(path: "plug")
        await assertThrows(.missingEmbeddedExecutable(missing)) {
            _ = try await AppInstallationInspector.readEmbeddedVersion(at: missing)
        }
    }

    func testBundleInfoIsReadFromTheBundlesInfoPlist() throws {
        let bundle = try temporaryDirectory().appending(path: "Plug.app")
        let contents = bundle.appending(path: "Contents")
        try FileManager.default.createDirectory(at: contents, withIntermediateDirectories: true)
        let info = ["CFBundleIdentifier": "com.example.inspector-\(UUID().uuidString)",
                    "CFBundleShortVersionString": "0.9.1", "CFBundleVersion": "42"]
        try PropertyListSerialization.data(fromPropertyList: info, format: .xml, options: 0)
            .write(to: contents.appending(path: "Info.plist"))

        XCTAssertEqual(
            try AppInstallationInspector.readBundleInfo(at: bundle),
            ["CFBundleShortVersionString": "0.9.1", "CFBundleVersion": "42"]
        )
    }

    /// A bundle nobody signed never passes as Plug, whether the reader
    /// throws or says it is not valid.
    func testAnUnsignedBundleIsNotAValidSignature() throws {
        let bundle = try temporaryDirectory().appending(path: "Plug.app")
        try FileManager.default.createDirectory(
            at: bundle.appending(path: "Contents/MacOS"), withIntermediateDirectories: true
        )
        let evidence = try? AppInstallationInspector.readSignature(at: bundle)
        XCTAssertNotEqual(evidence?.valid, true)
    }

    private func fixture(
        bundleURL: URL = URL(fileURLWithPath: "/Applications/Plug.app"),
        valid: Bool = true,
        bundleIdentifier: String = AppInstallationInspector.bundleIdentifier,
        teamID: String = AppInstallationInspector.teamID,
        info: [String: String] = ["CFBundleShortVersionString": "0.7.0", "CFBundleVersion": "20"],
        embeddedVersion: String = "0.7.0"
    ) -> AppInstallationInspector {
        AppInstallationInspector(
            bundleURL: { bundleURL },
            signatureReader: { _ in
                AppSignatureEvidence(valid: valid, bundleIdentifier: bundleIdentifier, teamID: teamID)
            },
            infoReader: { _ in info },
            embeddedVersionReader: { _ in embeddedVersion }
        )
    }

    private func assertThrows(
        _ expected: AppInstallationError,
        operation: () async throws -> Void
    ) async {
        do {
            try await operation()
            XCTFail("Expected \(expected)")
        } catch {
            XCTAssertEqual(error as? AppInstallationError, expected)
        }
    }
}
