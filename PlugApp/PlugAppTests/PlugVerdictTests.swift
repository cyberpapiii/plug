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
        XCTAssertEqual(verdict.secondary?.intent, .checkup)
    }

    func testVersionMismatchAsksForARestartInPlainWords() {
        let verdict = PlugVerdict.verdict(for: PlugSituation(runtime: .versionMismatch))
        XCTAssertEqual(verdict.title, "Restart Plug to finish updating")
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

    /// A sign-in in progress used to drop its button, so a closed browser
    /// tab left nothing to press until the command gave up minutes later.
    func testSignInInProgressOffersTryAgainAndCancel() {
        let verdict = PlugVerdict.verdict(
            for: PlugSituation(
                runtime: .running,
                servers: [server("Notion", health: .signInNeeded, signingIn: true)]
            )
        )
        XCTAssertEqual(verdict.primary, .init("Try Again", .signIn(server: "Notion")))
        XCTAssertEqual(verdict.secondary, .init("Cancel", .cancelSignIn(server: "Notion")))
        XCTAssertEqual(verdict.detail, "Finish signing in with your browser.")
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
        XCTAssertEqual(verdict.secondary?.intent, .checkup)
        XCTAssertEqual(situation.troubledServers.map(\.name), ["a", "b"])
        XCTAssertEqual(situation.troubledServers.compactMap(\.fix).count, 2)
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

    func testEveryProblemCarriesItsOwnFix() {
        XCTAssertEqual(server("Notion", health: .signInNeeded).fix?.intent, .signIn(server: "Notion"))
        XCTAssertEqual(server("Notion", health: .signInNeeded).fix?.title, "Sign In")
        XCTAssertEqual(server("Notion", health: .signInNeeded, signingIn: true).fix?.title, "Try Again")
        XCTAssertEqual(server("Notion", health: .signInNeeded, signingIn: true).fix?.intent, .signIn(server: "Notion"))
        XCTAssertEqual(
            server("Notion", health: .signInNeeded, signingIn: true).cancelSignIn?.intent,
            .cancelSignIn(server: "Notion")
        )
        XCTAssertNil(server("Notion", health: .signInNeeded).cancelSignIn)
        XCTAssertEqual(server("Linear", health: .down).fix?.intent, .restartServer("Linear"))
        XCTAssertEqual(server("Odd", health: .unknown).fix?.intent, .restartServer("Odd"))
        for health in [ServerHealth.working, .starting, .off] {
            XCTAssertNil(server("fine", health: health).fix, "\(health) needs no fix")
        }
    }

    func testTheVerdictOffersTheServersOwnFix() {
        for troubled in [server("Notion", health: .signInNeeded), server("Linear", health: .down)] {
            let verdict = PlugVerdict.verdict(
                for: PlugSituation(runtime: .running, servers: [troubled, server("ok")])
            )
            XCTAssertEqual(verdict.primary, troubled.fix)
        }
    }

    func testToolCountReadsAsWords() {
        XCTAssertEqual(server("a", tools: 1).toolCountText, "1 tool")
        XCTAssertEqual(server("a", tools: 0).toolCountText, "0 tools")
        XCTAssertEqual(server("a", tools: 12).toolCountText, "12 tools")
    }

    func testCharacterWearsTheFaceTheVerdictReads() {
        XCTAssertEqual(PlugCharacter.Mood(.good), .awake)
        XCTAssertEqual(PlugCharacter.Mood(.quiet), .asleep)
        XCTAssertEqual(PlugCharacter.Mood(.busy), .working)
        XCTAssertEqual(PlugCharacter.Mood(.attention), .needsYou)
        XCTAssertEqual(PlugCharacter.Mood(.blocked), .alert)

        func mood(_ servers: [ServerFacts]) -> PlugCharacter.Mood {
            PlugVerdict.verdict(for: PlugSituation(runtime: .running, servers: servers)).mood
        }
        XCTAssertEqual(mood([]), .curious)
        XCTAssertEqual(mood([server("a")]), .awake)
        XCTAssertEqual(mood([server("a", health: .signInNeeded), server("b")]), .needsYou)
        XCTAssertEqual(mood([server("a", health: .down), server("b")]), .worried)
        XCTAssertEqual(mood([server("a", health: .down), server("b", health: .signInNeeded)]), .dizzy)
        XCTAssertEqual(
            mood([server("a", health: .signInNeeded), server("b", health: .signInNeeded)]), .needsYou
        )
        XCTAssertEqual(PlugVerdict.verdict(for: PlugSituation(runtime: .off)).mood, .asleep)
        XCTAssertEqual(PlugVerdict.verdict(for: PlugSituation(runtime: .stopped)).mood, .out)
    }

    private func trouble(_ situation: PlugSituation) -> PanelTrouble? {
        PanelTrouble.trouble(for: situation, verdict: PlugVerdict.verdict(for: situation))
    }

    func testPanelHasNoCardWhenNothingNeedsYou() {
        XCTAssertNil(trouble(PlugSituation(runtime: .running, servers: [server("a")])))
        XCTAssertNil(trouble(PlugSituation(runtime: .running, servers: [server("a", health: .starting)])))
        XCTAssertNil(trouble(PlugSituation(runtime: .off)))
        XCTAssertNil(trouble(PlugSituation(runtime: .starting)))
    }

    func testOneTroubledServerIsTheOneBlueButton() throws {
        let card = try XCTUnwrap(trouble(PlugSituation(
            runtime: .running, servers: [server("Notion", health: .signInNeeded), server("b")]
        )))
        XCTAssertEqual(card.servers.map(\.name), ["Notion"])
        XCTAssertTrue(card.fixIsPrimary)
        XCTAssertNil(card.note)
        XCTAssertNil(card.primary)
        XCTAssertEqual(card.wash, .attention)

        let down = try XCTUnwrap(trouble(PlugSituation(runtime: .running, servers: [server("Figma", health: .down)])))
        XCTAssertEqual(down.wash, .blocked)
    }

    func testSeveralTroubledServersShareOneCheckupAndNoBlueButton() throws {
        let card = try XCTUnwrap(trouble(PlugSituation(runtime: .running, servers: [
            server("a", health: .signInNeeded), server("b", health: .down), server("c"),
        ])))
        XCTAssertEqual(card.servers.map(\.name), ["a", "b"])
        XCTAssertFalse(card.fixIsPrimary)
        XCTAssertNil(card.primary)
        XCTAssertEqual(card.secondary?.intent, .checkup)
        XCTAssertEqual(card.note, "1 of 3 servers running")
        XCTAssertEqual(card.icon, .checkup)
        XCTAssertEqual(card.wash, .blocked)
    }

    func testPanelShowsThreeTroubledServersAndCountsTheRest() throws {
        let servers = ["a", "b", "c", "d", "e"].map { server($0, health: .signInNeeded) } + [server("f")]
        let card = try XCTUnwrap(trouble(PlugSituation(runtime: .running, servers: servers)))
        XCTAssertEqual(card.servers.map(\.name), ["a", "b", "c"])
        XCTAssertEqual(card.note, "and 2 more · 1 of 6 servers running")
        XCTAssertEqual(card.wash, .attention)
    }

    func testStoppedPlugOffersOnlyStartPlug() throws {
        let card = try XCTUnwrap(trouble(PlugSituation(runtime: .stopped, servers: [server("a", health: .down)])))
        XCTAssertTrue(card.servers.isEmpty)
        XCTAssertEqual(card.primary?.intent, .reconnect)
        XCTAssertNil(card.secondary)
        XCTAssertEqual(card.icon, .plug)
        XCTAssertEqual(card.wash, .blocked)
    }

    func testNoServersInvitesTheFirstOneWithoutAlarm() throws {
        let card = try XCTUnwrap(trouble(PlugSituation(runtime: .running)))
        XCTAssertEqual(card.primary?.intent, .addServer)
        XCTAssertEqual(card.note, "Add your first one")
        XCTAssertEqual(card.icon, .addServer)
        XCTAssertNil(card.wash)
    }

    func testPlugNeedingSomethingPutsItsOwnButtonInTheCard() throws {
        let card = try XCTUnwrap(trouble(PlugSituation(setup: .needsPermission, runtime: .stopped)))
        XCTAssertEqual(card.primary?.intent, .allowBackgroundRunning)
        XCTAssertEqual(card.wash, .attention)
    }

    func testTroubledServerSaysWhatIsWrongInAFewWords() {
        XCTAssertEqual(server("a", health: .signInNeeded).reason, "Needs sign-in")
        XCTAssertEqual(
            server("a", health: .signInNeeded, signingIn: true).reason, "Finish signing in with your browser"
        )
        XCTAssertEqual(server("a", health: .down).reason, "Stopped")
        XCTAssertEqual(server("a", health: .down, error: "connection refused").reason, "connection refused")
    }

    func testRunningSummaryCountsWhatIsUpOrHowFarAlong() {
        let up = PlugSituation(runtime: .running, servers: [
            server("a", tools: 3), server("b", tools: 4), server("c", health: .down, tools: 0),
        ])
        XCTAssertEqual(up.runningSummary(stale: false), "2 running · 7 tools")
        XCTAssertEqual(up.runningSummary(stale: true), "Last known · 2 running · 7 tools")
        let starting = PlugSituation(runtime: .running, servers: [server("a"), server("b", health: .starting)])
        XCTAssertEqual(starting.runningSummary(stale: false), "1 of 2 ready")
        XCTAssertEqual(starting.settledServers.map(\.name), ["a", "b"])
    }

    /// A face that never came to rest would redraw for ever, and one that
    /// moved a part out of sight would not be the character.
    func testEveryFaceHoldsStillAsAWholeCharacter() {
        for mood in PlugCharacter.Mood.allCases {
            let pose = PlugPose.pose(for: mood, at: 0, moving: false)
            XCTAssertEqual(pose, PlugPose.pose(for: mood, at: 3.7, moving: false), "\(mood)")
            for part in [PlugPose.Part.bodyWidth, .bodyHeight, .leftProngHeight, .rightProngHeight] {
                XCTAssertGreaterThan(pose[part], 0, "\(mood) \(part)")
            }
        }
    }

    func testMenuBarIconChangesShapeNotJustColour() {
        let symbols = [
            PlugVerdict.menuBarMark(for: PlugVerdict.verdict(for: PlugSituation(runtime: .running, servers: [server("a")]))),
            PlugVerdict.menuBarMark(for: PlugVerdict.verdict(for: PlugSituation(runtime: .running, servers: [server("a", health: .down)]))),
            PlugVerdict.menuBarMark(for: PlugVerdict.verdict(for: PlugSituation(runtime: .stopped)))
        ]
        XCTAssertEqual(Set(symbols).count, 3)
        // Every state has a mark of its own.
        let tones: [Verdict.Tone] = [.good, .quiet, .busy, .attention, .blocked]
        let marks = tones.map {
            PlugVerdict.menuBarMark(for: Verdict(tone: $0, title: "", detail: ""))
        }
        XCTAssertEqual(Set(marks).count, tones.count)
    }

    func testHealthNeverLeaksProtocolVocabulary() {
        XCTAssertEqual(ServerHealth(daemonValue: "AuthRequired", enabled: true).label, "Sign-in needed")
        XCTAssertEqual(ServerHealth(daemonValue: "Failed", enabled: true).label, "Down")
        XCTAssertEqual(ServerHealth(daemonValue: "Healthy", enabled: false), .off)
        XCTAssertEqual(ServerHealth(daemonValue: nil, enabled: true), .starting)
    }

    func testDegradedServerStillReadsAsRunning() {
        let health = ServerHealth(daemonValue: "Degraded", enabled: true)
        XCTAssertEqual(health, .working)
        XCTAssertFalse(health.needsAttention)
    }

    func testWorkingHealthUsesLiveDotInsteadOfCompletionCheckmark() {
        XCTAssertEqual(ServerHealth(daemonValue: "Healthy", enabled: true).symbol, "circle.fill")
    }

    /// Only a state that is changing moves. A running server used to pulse,
    /// so a healthy list looked busy.
    func testOnlySettlingServersPulse() {
        XCTAssertTrue(ServerHealth.starting.pulses)
        for health in [ServerHealth.working, .signInNeeded, .down, .notLoaded, .off, .unknown] {
            XCTAssertFalse(health.pulses, "\(health) should hold still")
        }
    }

    /// A server with no runtime status used to read "Starting" forever. After
    /// the daemon has been up a minute, silence means it was never loaded.
    func testAServerTheDaemonNeverLoadedSaysSoAndOffersAReload() {
        XCTAssertEqual(ServerHealth(daemonValue: nil, enabled: true, daemonUptimeSecs: 5), .starting)
        XCTAssertEqual(ServerHealth(daemonValue: nil, enabled: true, daemonUptimeSecs: 61), .notLoaded)
        XCTAssertEqual(ServerHealth(daemonValue: "Starting", enabled: true, daemonUptimeSecs: 600), .starting)
        XCTAssertEqual(ServerHealth(daemonValue: nil, enabled: false, daemonUptimeSecs: 600), .off)

        let missing = server("Linear", health: .notLoaded)
        XCTAssertEqual(missing.fix, .init("Reload", .reloadConfiguration))
        let verdict = PlugVerdict.verdict(
            for: PlugSituation(runtime: .running, servers: [missing, server("ok")])
        )
        XCTAssertEqual(verdict.title, "Linear is not loaded")
        XCTAssertEqual(verdict.primary?.intent, .reloadConfiguration)
        XCTAssertEqual(verdict.tone, .attention)
    }

    /// The panel's rows used to look current after the daemon went away. The
    /// count line counts the servers that are on, as the verdict does.
    func testThePanelAndTheWindowShareOneCountLine() {
        let off = server("b", health: .off, enabled: false, tools: 9)
        let situation = PlugSituation(runtime: .reconnecting, servers: [off, server("a", tools: 3)])
        XCTAssertEqual(situation.countsSummary(stale: false), "1 server · 3 tools")
        XCTAssertEqual(situation.countsSummary(stale: true), "Last known · 1 server · 3 tools")
        XCTAssertEqual(situation.listedServers.map(\.name), ["a", "b"])
        XCTAssertEqual(
            PlugSituation(servers: [server("a", tools: 1)]).countsSummary(stale: false),
            "1 server · 1 tool"
        )
    }

    func testConnectedAppsGroupSessionsByAppInFirstSeenOrder() {
        let targets = AppIcons.distinctTargets(forClientTypes: [
            "claude-code", "Codex CLI", "claude-code", "Claude Desktop"
        ])
        XCTAssertEqual(targets, ["claude-code", "codex-cli", "claude-desktop"])
        let situation = PlugSituation(
            runtime: .running,
            connectedApps: targets.count,
            connectedAppTargets: targets
        )
        XCTAssertEqual(situation.connectedApps, 3)
    }

    func testThePanelNamesWhoIsConnected() {
        func connected(_ names: [String]) -> String {
            PlugSituation(
                connectedApps: names.count,
                connectedClients: names.map { ConnectedClient(target: $0, name: $0) }
            ).connectedSummary
        }
        XCTAssertEqual(connected([]), "No clients connected")
        XCTAssertEqual(connected(["Claude"]), "Claude connected")
        XCTAssertEqual(connected(["Claude", "Codex"]), "Claude and Codex connected")
        XCTAssertEqual(connected(["Claude", "Codex", "Cursor"]), "Claude, Codex and 1 other connected")
        XCTAssertEqual(connected(["Claude", "Codex", "Cursor", "Zed", "Pi"]), "Claude, Codex and 3 others connected")
        XCTAssertEqual(connected(["Claude", "Claude", "Claude"]), "Claude and 2 others connected")
        XCTAssertEqual(connected(["", ""]), "2 clients connected")
    }

    func testThePanelCountsServersThatAreOff() {
        let situation = PlugSituation(runtime: .running, servers: [
            ServerFacts(name: "a", enabled: true, transport: "stdio", health: .working, toolCount: 3),
            ServerFacts(name: "b", enabled: false, transport: "stdio", health: .off, toolCount: 0),
        ])
        XCTAssertEqual(situation.offServers.map(\.name), ["b"])
        XCTAssertEqual(situation.runningSummary(stale: false), "1 running · 3 tools · 1 off")
    }

    func testThePanelSaysHowEventsAre() {
        XCTAssertEqual(PlugSituation.eventsSummary(count: 1, failing: 0), "1 event")
        XCTAssertEqual(PlugSituation.eventsSummary(count: 3, failing: 1), "3 events · 1 needs attention")
        XCTAssertEqual(PlugSituation.eventsSummary(count: 3, failing: 2), "3 events · 2 need attention")
    }
}
