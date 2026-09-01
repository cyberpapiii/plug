import XCTest
import PlugIPC
@testable import Plug

/// The checkup is the app's version of a question people used to have to ask in
/// a terminal, so these pin what it reads and what it says about the answer.
final class CheckupTests: XCTestCase {
    private func checkup(_ json: String) throws -> Checkup {
        try JSONDecoder().decode(Checkup.self, from: Data(json.utf8))
    }

    func testReadsTheCheckupTheRuntimePrints() throws {
        let result = try checkup(
            """
            {"checks":[
              {"name":"config_exists","status":"Pass","message":"Config file valid","fix_suggestion":null},
              {"name":"client_limits","status":"Warn","message":"Too many tools","fix_suggestion":"Filter some"}
            ],"exit_code":2}
            """
        )
        XCTAssertEqual(result.checks.count, 2)
        XCTAssertEqual(result.checks[0].result, .pass)
        XCTAssertEqual(result.checks[1].result, .warn)
        XCTAssertEqual(result.checks[1].fix, "Filter some")
    }

    func testAnUnknownStatusCountsAsAProblemRatherThanAPass() throws {
        let result = try checkup(#"{"checks":[{"name":"port_available","status":"Fail","message":"Port busy"}]}"#)
        XCTAssertEqual(result.checks[0].result, .fail)
        XCTAssertNil(result.checks[0].fix)
    }

    func testIdentifiersBecomeTitlesAPersonCanRead() {
        XCTAssertEqual(Check(name: "config_permissions", result: .pass, message: "").title, "Settings file is private")
        XCTAssertEqual(Check(name: "server_binaries", result: .pass, message: "").title, "Server programs")
        XCTAssertEqual(Check(name: "brand_new_check", result: .pass, message: "").title, "Brand New Check")
    }

    func testACleanCheckupSaysHowMuchWasChecked() {
        let clean = Checkup(checks: [
            Check(name: "a", result: .pass, message: ""),
            Check(name: "b", result: .pass, message: ""),
        ])
        XCTAssertTrue(clean.isClean)
        XCTAssertEqual(clean.headline, "All 2 checks passed")
    }

    func testProblemsAndWarningsAreCountedSeparately() {
        let mixed = Checkup(checks: [
            Check(name: "a", result: .pass, message: ""),
            Check(name: "b", result: .warn, message: ""),
            Check(name: "c", result: .fail, message: ""),
            Check(name: "d", result: .warn, message: ""),
        ])
        XCTAssertFalse(mixed.isClean)
        XCTAssertEqual(mixed.headline, "1 problem, 2 warnings")
    }

    func testTroubleIsListedFirstBecauseThatIsWhyItWasRun() {
        let mixed = Checkup(checks: [
            Check(name: "pass", result: .pass, message: ""),
            Check(name: "warn", result: .warn, message: ""),
            Check(name: "fail", result: .fail, message: ""),
        ])
        XCTAssertEqual(mixed.ordered.map(\.name), ["fail", "warn", "pass"])
    }

    func testAnEmptyCheckupSaysNothingWasChecked() {
        XCTAssertEqual(Checkup(checks: []).headline, "Nothing was checked")
    }
}

/// Icons are the reason a row can be recognized before it is read, so the
/// fallbacks matter as much as the real thing.
final class AppIconTests: XCTestCase {
    func testCommandLineToolsGetATerminal() {
        XCTAssertEqual(AppIcons.symbol(target: "gemini-cli", name: "Gemini CLI"), "terminal")
    }

    func testClaudeAndCodexVariantsUseSharedVisualFallbacks() {
        XCTAssertEqual(
            AppIcons.symbol(target: "claude-code", name: "Claude Code"),
            AppIcons.symbol(target: "claude-desktop", name: "Claude Desktop")
        )
        XCTAssertEqual(
            AppIcons.symbol(target: "codex-cli", name: "Codex CLI"),
            AppIcons.symbol(target: "codex", name: "Codex")
        )
        XCTAssertEqual(AppIcons.symbol(target: "goose", name: "Goose"), "bird.fill")
    }

    func testEditorsGetAnEditorGlyph() {
        XCTAssertEqual(
            AppIcons.symbol(target: "vscode", name: "VS Code"),
            "chevron.left.forwardslash.chevron.right"
        )
    }

    func testAnUnknownAppStillGetsSomethingAppShaped() {
        XCTAssertEqual(AppIcons.symbol(target: "brand-new-thing", name: "Brand New Thing"), "app.dashed")
    }

    func testLiveSessionsAreMatchedToTheAppTheyBelongTo() {
        XCTAssertEqual(AppIcons.target(forClientType: "claude_code"), "claude-code")
        XCTAssertEqual(AppIcons.target(forClientType: "Claude Desktop"), "claude-desktop")
        XCTAssertEqual(AppIcons.target(forClientType: "Codex CLI"), "codex-cli")
        XCTAssertEqual(AppIcons.target(forClientType: "codex-mcp-client"), "codex-cli")
        XCTAssertEqual(AppIcons.target(forClientType: "Devin"), "windsurf")
        XCTAssertEqual(AppIcons.target(forClientType: "Cascade (Devin Desktop)"), "windsurf")
        XCTAssertEqual(AppIcons.displayName(forTarget: "windsurf"), "Devin")
        XCTAssertEqual(AppIcons.target(forClientType: "cursor"), "cursor")
        XCTAssertEqual(AppIcons.target(forClientType: "Hermes Agent"), "hermes-agent")
        XCTAssertNil(AppIcons.displayName(forTarget: "hermes-agent"))
    }

    func testAnUnrecognizedClientTypeIsPassedThroughRatherThanGuessed() {
        XCTAssertEqual(AppIcons.target(forClientType: "some_new_client"), "some-new-client")
    }
}

/// Reloading says what moved. These pin the sentence.
final class ReloadSummaryTests: XCTestCase {
    func testNamesWhatChanged() {
        let summary = ReloadSummary(added: ["figma"], removed: [], changed: ["notion", "exa"])
        XCTAssertEqual(summary.summary, "1 added, 2 changed")
    }

    func testAReloadThatChangedNothingSaysSo() {
        XCTAssertEqual(ReloadSummary().summary, "Nothing changed")
    }

    func testDecodesAReportWithFieldsMissing() throws {
        let summary = try JSONDecoder().decode(ReloadSummary.self, from: Data(#"{"added":["a"]}"#.utf8))
        XCTAssertEqual(summary.added, ["a"])
        XCTAssertTrue(summary.removed.isEmpty)
        XCTAssertEqual(summary.summary, "1 added")
    }
}

/// A server row says where the server runs before it says it in words.
final class ServerGlyphTests: XCTestCase {
    private func server(transport: String, enabled: Bool = true, health: ServerHealth = .working, error: String? = nil) -> ServerFacts {
        ServerFacts(name: "s", enabled: enabled, transport: transport, health: health, error: error)
    }

    func testLocalAndRemoteLookDifferent() {
        XCTAssertEqual(server(transport: "stdio").transportSymbol, "desktopcomputer")
        XCTAssertEqual(server(transport: "streamable_http").transportSymbol, "globe")
        XCTAssertEqual(server(transport: "sse").transportLabel, "Remote server")
    }

    func testASwitchedOffServerSaysSoWithItsOwnGlyph() {
        XCTAssertEqual(server(transport: "stdio", enabled: false).subtitleSymbol, "circle.slash")
    }

    func testAFailingServerShowsTroubleRatherThanItsTransport() {
        let failing = server(transport: "stdio", health: .down, error: "connection refused")
        XCTAssertEqual(failing.subtitleSymbol, "exclamationmark.triangle")
    }
}
