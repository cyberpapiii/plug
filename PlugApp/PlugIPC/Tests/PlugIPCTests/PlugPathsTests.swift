import Foundation
import XCTest
@testable import PlugIPC

/// `testdata/paths.json` is the daemon's names; `plug-core/src/paths.rs`
/// checks the same file.
final class PlugPathsTests: XCTestCase {
    private struct Fixture: Decodable {
        let dir: String
        let logDir: String
        let socket: String
        let token: String
        let serviceDisabledMarker: String
        let menuBarHiddenMarker: String
        let daemonLabel: String
        let appBundleId: String
        let teamId: String
        let bundledBinary: String
        let daemonArgs: [String]
        let commandLink: String
    }

    func testTheAppUsesTheDaemonsNames() throws {
        let repoRoot = URL(fileURLWithPath: "\(#filePath)")
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let fixture = try decoder.decode(
            Fixture.self,
            from: Data(contentsOf: repoRoot.appending(path: "testdata/paths.json"))
        )

        XCTAssertEqual(fixture.dir, PlugPaths.directory)
        XCTAssertEqual(fixture.logDir, PlugPaths.logDirectory)
        XCTAssertEqual(fixture.socket, PlugPaths.socketFile)
        XCTAssertEqual(fixture.token, PlugPaths.tokenFile)
        XCTAssertEqual(fixture.serviceDisabledMarker, PlugPaths.serviceDisabledMarker)
        XCTAssertEqual(fixture.menuBarHiddenMarker, PlugPaths.menuBarHiddenMarker)
        XCTAssertEqual(fixture.daemonLabel, PlugPaths.daemonLabel)
        XCTAssertEqual(fixture.appBundleId, PlugPaths.appBundleID)
        XCTAssertEqual(fixture.teamId, PlugPaths.teamID)
        XCTAssertEqual(fixture.bundledBinary, PlugPaths.bundledBinary)
        XCTAssertEqual(fixture.daemonArgs, PlugPaths.daemonArguments)
        XCTAssertEqual(fixture.commandLink, PlugPaths.commandLink)
    }

    /// The plist the app registers names the job and runs the binary the way
    /// the daemon and both inspectors expect, or every app-managed job reads
    /// as someone else's.
    func testTheAppsLaunchAgentRunsTheDaemonAsNamed() throws {
        let repoRoot = URL(fileURLWithPath: "\(#filePath)")
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let data = try Data(
            contentsOf: repoRoot.appending(path: "PlugApp/PlugApp/Resources/\(PlugPaths.daemonLabel).plist")
        )
        let plist = try XCTUnwrap(
            PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any]
        )
        XCTAssertEqual(plist["Label"] as? String, PlugPaths.daemonLabel)
        XCTAssertEqual(plist["BundleProgram"] as? String, PlugPaths.bundledBinary)
        XCTAssertEqual(
            plist["ProgramArguments"] as? [String],
            [PlugPaths.bundledBinary] + PlugPaths.daemonArguments
        )
    }

    func testMarkersSitBesideTheSocket() {
        XCTAssertEqual(
            PlugPaths.marker(PlugPaths.menuBarHiddenMarker).deletingLastPathComponent(),
            PlugPaths.socketURL.deletingLastPathComponent()
        )
        XCTAssertTrue(PlugPaths.socketURL.path.hasSuffix("Library/Application Support/plug/plug.sock"))
    }
}
