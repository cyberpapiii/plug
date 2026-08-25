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
            XCTAssertEqual(installation.embeddedVersion, "0.7.0")
            XCTAssertEqual(installation.teamID, "HJF7LN64XX")
        }
    }

    func testRejectsWrongDeveloperTeam() async {
        let inspector = fixture(teamID: "ATTACKER123")

        await assertThrows(.untrustedTeam("ATTACKER123")) {
            _ = try await inspector.inspectCurrentApp()
        }
    }

    func testRejectsAppAndEmbeddedBinaryVersionMismatch() async {
        let inspector = fixture(embeddedVersion: "0.6.4")

        await assertThrows(.versionMismatch(app: "0.7.0", embedded: "0.6.4")) {
            _ = try await inspector.inspectCurrentApp()
        }
    }

    private func fixture(
        bundleURL: URL = URL(fileURLWithPath: "/Applications/Plug.app"),
        teamID: String = "HJF7LN64XX",
        embeddedVersion: String = "0.7.0"
    ) -> AppInstallationInspector {
        AppInstallationInspector(
            bundleURL: { bundleURL },
            signatureReader: { _ in
                AppSignatureEvidence(
                    valid: true,
                    bundleIdentifier: "com.cyberpapiii.plug",
                    teamID: teamID
                )
            },
            infoReader: { _ in
                ["CFBundleShortVersionString": "0.7.0", "CFBundleVersion": "20"]
            },
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
