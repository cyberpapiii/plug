import XCTest
@testable import Plug

/// The verdict is the only thing the menu bar icon, the popover headline, and
/// the window banner say, so its priority order is the product. These pin it.
final class PlugVerdictTests: XCTestCase {
    private func server(
        _ name: String,
        health: ServerHealth = .working,
        enabled: Bool = true,
        tools: Int = 10,
        error: String? = nil,
        signingIn: Bool = false
    ) -> ServerFacts {
        ServerFacts(
            name: name,
            enabled: enabled,
            transport: "stdio",
            usesOAuth: health == .signInNeeded,
            health: health,
            toolCount: tools,
            error: error,
            isSigningIn: signingIn
        )
    }

    func testHealthyRuntimeReportsWorkingWithCounts() {
        let verdict = PlugVerdict.verdict(
            for: PlugSituation(
                runtime: .running,
                servers: [server("a", tools: 3), server("b", tools: 4)]
            )
        )
        XCTAssertEqual(verdict.tone, .good)
        XCTAssertEqual(verdict.title, "All servers running")
        XCTAssertEqual(verdict.detail, "2 servers · 7 tools")
        XCTAssertNil(verdict.primary)
    }

    func testBlockedSetupOutranksEverythingElse() {
        let verdict = PlugVerdict.verdict(
            for: PlugSituation(
                setup: .blocked(detail: "Shell command points somewhere else.", hasLog: true),
                runtime: .stopped,
                servers: [server("a", health: .down)]
            )
        )
        XCTAssertEqual(verdict.tone, .blocked)
        XCTAssertEqual(verdict.primary?.intent, .repairInstallation)
        XCTAssertEqual(verdict.secondary?.intent, .showRepairLog)
        XCTAssertEqual(verdict.detail, "Shell command points somewhere else.")
    }

    func testMissingLogHidesTheLogButton() {
        let verdict = PlugVerdict.verdict(
            for: PlugSituation(setup: .blocked(detail: "No log.", hasLog: false), runtime: .stopped)
        )
        XCTAssertNil(verdict.secondary)
    }

    func testPermissionRequestOutranksAStoppedRuntime() {
        let verdict = PlugVerdict.verdict(
            for: PlugSituation(setup: .needsPermission, runtime: .stopped)
        )
        XCTAssertEqual(verdict.primary?.intent, .allowBackgroundRunning)
    }

    func testStoppedRuntimeOutranksServerTrouble() {
        let verdict = PlugVerdict.verdict(
            for: PlugSituation(runtime: .stopped, servers: [server("a", health: .down)])
        )
        XCTAssertEqual(verdict.title, "Plug is not running")
        XCTAssertEqual(verdict.primary?.intent, .reconnect)
    }

    func testVersionMismatchAsksForARestartInPlainWords() {
        let verdict = PlugVerdict.verdict(for: PlugSituation(runtime: .versionMismatch))
        XCTAssertEqual(verdict.title, "Restart required to finish update")
        XCTAssertEqual(verdict.primary?.intent, .reconnect)
    }

    func testSingleSignInProblemNamesTheServerAndOffersTheFix() {
        let verdict = PlugVerdict.verdict(
            for: PlugSituation(
                runtime: .running,
                servers: [server("Notion", health: .signInNeeded), server("Figma")]
            )
        )
        XCTAssertEqual(verdict.title, "Notion needs sign-in")
        XCTAssertEqual(verdict.primary?.intent, .signIn(server: "Notion"))
    }

    func testSignInInProgressDropsTheButtonAndPointsAtTheBrowser() {
        let verdict = PlugVerdict.verdict(
            for: PlugSituation(
                runtime: .running,
                servers: [server("Notion", health: .signInNeeded, signingIn: true)]
            )
        )
        XCTAssertNil(verdict.primary)
        XCTAssertEqual(verdict.detail, "Sign-in is open in the browser.")
    }

    func testSingleDownServerOffersRestart() {
        let verdict = PlugVerdict.verdict(
            for: PlugSituation(
                runtime: .running,
                servers: [server("Linear", health: .down, error: "connection refused")]
            )
        )
        XCTAssertEqual(verdict.title, "Linear is down")
        XCTAssertEqual(verdict.detail, "connection refused")
        XCTAssertEqual(verdict.primary?.intent, .restartServer("Linear"))
    }

    func testSeveralProblemsCountThemAndLeaveFixesToTheList() {
        let situation = PlugSituation(
            runtime: .running,
            servers: [
                server("a", health: .down),
                server("b", health: .signInNeeded),
                server("c")
            ]
        )
        let verdict = PlugVerdict.verdict(for: situation)
        XCTAssertEqual(verdict.title, "2 servers need attention")
        XCTAssertNil(verdict.primary)
        XCTAssertEqual(PlugVerdict.attention(for: situation).count, 2)
    }

    func testDisabledServersAreNeverTrouble() {
        let verdict = PlugVerdict.verdict(
            for: PlugSituation(
                runtime: .running,
                servers: [server("a"), server("off", health: .off, enabled: false)]
            )
        )
        XCTAssertEqual(verdict.tone, .good)
        XCTAssertEqual(verdict.detail, "1 server · 10 tools")
    }

    func testStartingServersReadAsBusyNotBroken() {
        let verdict = PlugVerdict.verdict(
            for: PlugSituation(
                runtime: .running,
                servers: [server("a"), server("b", health: .starting, tools: 0)]
            )
        )
        XCTAssertEqual(verdict.tone, .busy)
        XCTAssertEqual(verdict.detail, "1 of 2 ready.")
    }

    func testNoServersInvitesAddingOne() {
        let verdict = PlugVerdict.verdict(for: PlugSituation(runtime: .running))
        XCTAssertEqual(verdict.primary?.intent, .addServer)
    }

    func testAttentionItemsPairEveryProblemWithItsOwnFix() {
        let items = PlugVerdict.attention(
            for: PlugSituation(
                runtime: .running,
                servers: [
                    server("Notion", health: .signInNeeded),
                    server("Linear", health: .down, error: "spawn failed\nsecond line")
                ]
            )
        )
        XCTAssertEqual(items.map(\.id), ["Notion", "Linear"])
        XCTAssertEqual(items[0].button?.intent, .signIn(server: "Notion"))
        XCTAssertEqual(items[1].button?.intent, .restartServer("Linear"))
        XCTAssertEqual(items[1].detail, "spawn failed")
    }

    func testMenuBarIconChangesShapeNotJustColour() {
        let symbols = [
            PlugVerdict.menuBarSymbol(for: PlugVerdict.verdict(for: PlugSituation(runtime: .running, servers: [server("a")]))),
            PlugVerdict.menuBarSymbol(for: PlugVerdict.verdict(for: PlugSituation(runtime: .running, servers: [server("a", health: .down)]))),
            PlugVerdict.menuBarSymbol(for: PlugVerdict.verdict(for: PlugSituation(runtime: .stopped)))
        ]
        XCTAssertEqual(Set(symbols).count, 3)
    }

    func testHealthNeverLeaksProtocolVocabulary() {
        XCTAssertEqual(ServerHealth(daemonValue: "AuthRequired", enabled: true).label, "Sign-in needed")
        XCTAssertEqual(ServerHealth(daemonValue: "Failed", enabled: true).label, "Down")
        XCTAssertEqual(ServerHealth(daemonValue: "Healthy", enabled: false), .off)
        XCTAssertEqual(ServerHealth(daemonValue: nil, enabled: true), .starting)
    }

    func testWorkingHealthUsesLiveDotInsteadOfCompletionCheckmark() {
        XCTAssertEqual(ServerHealth(daemonValue: "Healthy", enabled: true).symbol, "circle.fill")
    }
}
