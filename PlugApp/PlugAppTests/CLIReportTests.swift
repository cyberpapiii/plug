import Foundation
import XCTest
@testable import Plug

/// The app's readers of `plug clients`, `plug import` and `plug doctor`
/// against what those commands print, from `testdata/cli`. A field renamed on
/// the CLI side fails here instead of decoding as a default.
final class CLIReportTests: XCTestCase {
    private let executable = URL(fileURLWithPath: "/Applications/Plug.app/Contents/Resources/plug")

    func testTheClientListIsReadAsPrinted() async throws {
        let runner = StubProcessRunner(result: try .golden("clients.json"))
        let apps = try await AppLinkService(runner: runner, executable: executable).apps()

        XCTAssertEqual(apps.map(\.target), ["claude-code", "cursor", "zed"])
        XCTAssertEqual(apps.map(\.name), ["Claude Code", "Cursor", "Zed"])
        XCTAssertEqual(apps.map(\.linked), [true, true, false])
        XCTAssertEqual(apps.map(\.detected), [true, true, true])
        XCTAssertEqual(apps.map(\.live), [true, false, false])
        XCTAssertEqual(apps.map(\.sessions), [2, 0, 0])
        XCTAssertEqual(apps.map(\.transport), ["stdio", "http", nil])
        let asked = await runner.arguments
        XCTAssertEqual(asked, [["clients", "--output", "json"]])
    }

    func testTheImportReportIsReadAsPrinted() async throws {
        let runner = StubProcessRunner(result: try .golden("import.json"))
        let scan = try await ImportService(runner: runner, executable: executable).scan()

        XCTAssertEqual(scan.servers.map(\.name), ["github", "notes"])
        XCTAssertEqual(scan.servers.map(\.source), ["cursor", "vscode"])
        XCTAssertEqual(scan.servers.map(\.sourceName), ["Cursor", "VS Code Copilot"])
        XCTAssertEqual(scan.servers[0].config.command, "npx")
        XCTAssertEqual(scan.servers[0].config.env, ["GITHUB_TOKEN": "$GITHUB_TOKEN"])
        XCTAssertEqual(scan.servers[0].config.timeoutSecs, 90)
        XCTAssertEqual(scan.servers[1].config.transport, "sse")
        XCTAssertEqual(scan.servers[1].detail, "https://notes.example.com/sse")
        XCTAssertEqual(scan.unreadable, ["Zed"])
    }

    func testTheCheckupIsReadAsPrinted() async throws {
        let runner = StubProcessRunner(result: try .golden("doctor.json", status: 1))
        let checkup = try await CheckupService(runner: runner, executable: executable).run()

        XCTAssertEqual(checkup.checks.map(\.name), ["config_exists", "port_available", "server_connectivity"])
        XCTAssertEqual(checkup.checks.map(\.result), [.pass, .warn, .fail])
        XCTAssertEqual(checkup.checks.map(\.title), ["Settings file", "Network port", "Servers responding"])
        XCTAssertEqual(checkup.checks[0].message, "config_exists message")
        XCTAssertEqual(checkup.checks.map(\.fix), [nil, "Free the port.", "Restart it."])
    }
}
