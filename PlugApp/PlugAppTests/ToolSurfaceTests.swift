import XCTest
import PlugIPC
@testable import Plug

/// The tool list is the surface where someone switches a single capability off,
/// so these pin what a search finds, how a name reads, and which tools are
/// honestly out of reach because a wildcard covers them.
final class ToolCatalogTests: XCTestCase {
    private let catalog = ToolCatalog([
        ToolFacts(name: "figma__get_file", server: "figma", summary: "Read a design file"),
        ToolFacts(name: "figma__export_frame", server: "figma", summary: "Export an image"),
        ToolFacts(
            name: "notion__search",
            server: "notion",
            summary: "Search pages",
            isOn: false
        ),
        ToolFacts(
            name: "notion__append_block",
            server: "notion",
            isOn: false,
            lockedByPattern: "notion__append*"
        ),
    ])

    func testShortNameDropsTheServerPrefixTheGroupAlreadyStates() {
        XCTAssertEqual(
            ToolFacts(name: "figma__get_file", server: "figma").shortName,
            "get_file"
        )
    }

    func testShortNameKeepsNamesThatDoNotCarryThePrefix() {
        XCTAssertEqual(ToolFacts(name: "search", server: "notion").shortName, "search")
    }

    func testShortNameMatchesThePrefixRegardlessOfCase() {
        XCTAssertEqual(
            ToolFacts(name: "Figma__Get_File", server: "figma").shortName,
            "Get_File"
        )
    }

    func testGroupsAreServerAlphabeticalAndToolAlphabetical() {
        let groups = catalog.groups()
        XCTAssertEqual(groups.map(\.server), ["figma", "notion"])
        XCTAssertEqual(groups[0].tools.map(\.shortName), ["export_frame", "get_file"])
    }

    func testSearchingForAServerShowsEverythingThatServerCanDo() {
        let groups = catalog.groups(matching: "figma")
        XCTAssertEqual(groups.count, 1)
        XCTAssertEqual(groups[0].tools.count, 2)
    }

    func testSearchingMatchesDescriptionsNotJustNames() {
        let groups = catalog.groups(matching: "export an image")
        XCTAssertEqual(groups.flatMap(\.tools).map(\.name), ["figma__export_frame"])
    }

    func testSearchNarrowsOneServersToolsButNamingTheServerKeepsAll() {
        XCTAssertEqual(
            catalog.tools(for: "figma", matching: "export").map(\.shortName),
            ["export_frame"]
        )
        XCTAssertEqual(catalog.tools(for: "figma", matching: "figma").count, 2)
        XCTAssertEqual(catalog.tools(for: "figma", matching: " ").count, 2)
    }

    func testSearchFindsTheServersThatHaveAMatchingTool() {
        XCTAssertEqual(catalog.servers(withToolsMatching: "export an image"), ["figma"])
        XCTAssertTrue(catalog.servers(withToolsMatching: "").isEmpty)
        XCTAssertTrue(catalog.servers(withToolsMatching: "no such tool").isEmpty)
    }

    func testSearchIgnoresSurroundingSpaceAndCase() {
        XCTAssertEqual(catalog.groups(matching: "  NOTION "), catalog.groups(matching: "notion"))
    }

    func testCountsSeparateOnFromOff() {
        XCTAssertEqual(catalog.onCount, 2)
        XCTAssertEqual(catalog.offCount, 2)
    }

    func testAGroupWithNothingLeftOnSaysSo() {
        let notion = catalog.groups(matching: "notion")[0]
        XCTAssertEqual(notion.onCount, 0)
        XCTAssertTrue(notion.isFullyOff)
        XCTAssertFalse(catalog.groups(matching: "figma")[0].isFullyOff)
    }

    func testAToolCoveredByAWildcardCannotBeSwitchedBackOnAlone() {
        let covered = catalog.tools(for: "notion").first { $0.lockedByPattern != nil }
        XCTAssertEqual(covered?.lockedByPattern, "notion__append*")
        XCTAssertEqual(covered?.canToggle, false)
        XCTAssertEqual(catalog.tools(for: "notion").first { $0.name.hasSuffix("search") }?.canToggle, true)
    }

    func testDaemonToolsBecomeFactsWithoutLosingWhySomethingIsOff() {
        let facts = ToolFacts(
            ToolInfo(
                name: "notion__append_block",
                serverId: "notion",
                title: "Append block",
                disabled: true,
                disabledByPattern: "notion__append*"
            )
        )
        XCTAssertEqual(facts.server, "notion")
        XCTAssertEqual(facts.summary, "Append block")
        XCTAssertFalse(facts.isOn)
        XCTAssertEqual(facts.lockedByPattern, "notion__append*")
    }

    func testDescriptionIsPreferredOverTitleWhenBothArrive() {
        let facts = ToolFacts(
            ToolInfo(name: "figma__get_file", serverId: "figma", description: "Read a design file", title: "Get file")
        )
        XCTAssertEqual(facts.summary, "Read a design file")
        XCTAssertTrue(facts.isOn)
        XCTAssertNil(facts.lockedByPattern)
    }

    func testPatternListsEveryToolItCovers() {
        let catalog = ToolCatalog([
            ToolFacts(name: "notion__search", server: "notion", isOn: false, lockedByPattern: "notion__*"),
            ToolFacts(name: "notion__append", server: "notion", isOn: false, lockedByPattern: "notion__*"),
            ToolFacts(name: "figma__get_file", server: "figma"),
            ToolFacts(name: "figma__delete", server: "figma", isOn: false),
        ])
        XCTAssertEqual(
            catalog.tools(coveredBy: "notion__*").map(\.shortName),
            ["append", "search"]
        )
        XCTAssertTrue(catalog.tools(coveredBy: "figma__*").isEmpty)
    }
}

/// The app list comes from `plug clients --output json`. These pin the shape
/// that command actually prints, including the fields it omits.
final class LinkableAppTests: XCTestCase {
    private func decode(_ json: String) throws -> [LinkableApp] {
        struct Listing: Decodable { let clients: [LinkableApp] }
        return try JSONDecoder().decode(Listing.self, from: Data(json.utf8)).clients
    }

    func testDecodesTheListingTheCommandPrints() throws {
        let apps = try decode(
            """
            {"clients":[{"target":"claude-desktop","name":"Claude Desktop","linked":true,
            "detected":true,"live":true,"live_sessions":2,"linked_transport":"stdio"}]}
            """
        )
        XCTAssertEqual(apps.count, 1)
        XCTAssertEqual(apps[0].id, "claude-desktop")
        XCTAssertEqual(apps[0].name, "Claude Desktop")
        XCTAssertTrue(apps[0].linked)
        XCTAssertTrue(apps[0].detected)
        XCTAssertTrue(apps[0].live)
        XCTAssertEqual(apps[0].sessions, 2)
        XCTAssertEqual(apps[0].transport, "stdio")
    }

    func testAnAppWithOnlyATargetStillDecodes() throws {
        let apps = try decode(#"{"clients":[{"target":"cursor"}]}"#)
        XCTAssertEqual(apps[0].name, "cursor")
        XCTAssertFalse(apps[0].linked)
        XCTAssertFalse(apps[0].detected)
        XCTAssertFalse(apps[0].live)
        XCTAssertEqual(apps[0].sessions, 0)
        XCTAssertNil(apps[0].transport)
    }

    func testAnEmptyListingIsNotAnError() throws {
        XCTAssertTrue(try decode(#"{"clients":[]}"#).isEmpty)
    }
}

/// Environment variables are typed by hand into one field, so the parser has to
/// accept the shapes people type.
final class EditServerEnvironmentTests: XCTestCase {
    func testParsesOnePairPerLine() {
        XCTAssertEqual(
            EditServerView.parseEnvironment("API_KEY=abc\nREGION=us-east-1"),
            ["API_KEY": "abc", "REGION": "us-east-1"]
        )
    }

    func testCommasStayInsideValues() {
        XCTAssertEqual(
            EditServerView.parseEnvironment("A=1, B=2"),
            ["A": "1, B=2"]
        )
    }

    func testSpaceAroundTheNameAndValueIsTrimmed() {
        XCTAssertEqual(EditServerView.parseEnvironment("  TOKEN = xyz  "), ["TOKEN": "xyz"])
    }

    func testValuesKeepTheirOwnEqualsSigns() {
        XCTAssertEqual(EditServerView.parseEnvironment("URL=a=b=c"), ["URL": "a=b=c"])
    }

    func testAnEmptyValueIsKept() {
        XCTAssertEqual(EditServerView.parseEnvironment("EMPTY="), ["EMPTY": ""])
    }

    func testLinesWithoutAnEqualsOrANameAreSkipped() {
        XCTAssertEqual(EditServerView.parseEnvironment("nonsense\n=value\n\nA=1"), ["A": "1"])
    }

    func testNothingTypedMeansNoEnvironment() {
        XCTAssertTrue(EditServerView.parseEnvironment("   \n ").isEmpty)
    }

    func testSaveStaysDisabledWithoutServerConfigRead() {
        XCTAssertFalse(
            EditServerView.canSave(
                canReadServerConfig: false,
                loaded: true,
                isComplete: true,
                saving: false
            )
        )
        XCTAssertTrue(
            EditServerView.canSave(
                canReadServerConfig: true,
                loaded: true,
                isComplete: true,
                saving: false
            )
        )
    }

    func testMissingCapabilityCopyIsRestartUpdateNotParseError() {
        XCTAssertTrue(AppModel.serverConfigReadRequiredCopy.contains("Restart"))
        XCTAssertTrue(AppModel.serverConfigReadRequiredCopy.contains("update"))
        XCTAssertFalse(AppModel.serverConfigReadRequiredCopy.contains("PARSE_ERROR"))
        XCTAssertFalse(AppModel.serverConfigReadRequiredCopy.contains("could not be loaded"))
    }

    func testArgumentsRoundTripThroughTheDisplayedCommandLine() {
        let arguments = ["-y", "a package", "", "it's-safe", #"a\"quote"#]
        XCTAssertEqual(
            ServerDraftParser.tokenize(EditServerView.renderArguments(arguments)),
            arguments
        )
    }
}

/// The Clients screen places every client against the open sessions. These pin
/// which group an app lands in and that no session is counted twice or lost.
final class AppRosterTests: XCTestCase {
    private func apps(_ json: String) throws -> [LinkableApp] {
        try JSONDecoder().decode([LinkableApp].self, from: Data(json.utf8))
    }

    private func sessions(_ json: String) throws -> [LiveSession] {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode([LiveSession].self, from: Data(json.utf8))
    }

    func testAnAppWithAnOpenSessionIsConnectedAndOwnsIt() throws {
        let roster = AppRoster(
            apps: try apps(#"[{"target":"codex-cli","linked":true,"detected":true},{"target":"cursor","detected":true}]"#),
            sessions: try sessions(
                """
                [{"transport":"ipc","session_id":"a1","client_type":"codex","connected_secs":5},
                 {"transport":"ipc","session_id":"a2","client_type":"codex","connected_secs":9}]
                """
            )
        )
        XCTAssertEqual(roster.connected.map(\.app.target), ["codex-cli"])
        XCTAssertEqual(roster.connected[0].sessions.map(\.sessionId), ["a1", "a2"])
        XCTAssertEqual(roster.idle.map(\.target), ["cursor"])
        XCTAssertTrue(roster.other.isEmpty)
    }

    func testASessionFromNoKnownAppIsKeptApart() throws {
        let roster = AppRoster(
            apps: try apps(#"[{"target":"cursor","detected":true}]"#),
            sessions: try sessions(
                #"[{"transport":"ipc","session_id":"z9","client_type":"unknown","client_info":"ditto-history","connected_secs":1}]"#
            )
        )
        XCTAssertTrue(roster.connected.isEmpty)
        XCTAssertEqual(roster.other.map(\.sessionId), ["z9"])
    }

    func testAnUnknownSessionIsNamedAfterTheAppThatStartedIt() throws {
        let named = try sessions(
            """
            [{"transport":"daemon_proxy","session_id":"h1","client_type":"Unknown","client_info":"mcp","connected_secs":1,
              "host":{"name":"Hermes","executable":"/Applications/Hermes.app/Contents/MacOS/Hermes","app":"/Applications/Hermes.app"}},
             {"transport":"daemon_proxy","session_id":"h2","client_type":"Unknown","client_info":"ditto-history","connected_secs":1,
              "host":{"name":"python3","executable":"/usr/bin/python3"}},
             {"transport":"daemon_proxy","session_id":"h3","client_type":"Unknown","client_info":"mcp","connected_secs":1,
              "host":{"name":"python3","executable":"/usr/bin/python3"}},
             {"transport":"daemon_proxy","session_id":"h4","client_type":"Cursor","client_info":"cursor-vscode","connected_secs":1,
              "host":{"name":"Terminal","executable":"/x/Terminal.app/Contents/MacOS/Terminal","app":"/x/Terminal.app"}},
             {"transport":"daemon_proxy","session_id":"abcd9","client_type":"Unknown","client_info":"mcp","connected_secs":1}]
            """
        )
        XCTAssertEqual(
            named.map(\.displayName),
            ["Hermes", "ditto-history", "python3", "Cursor", "Unidentified local client abcd"]
        )
        XCTAssertEqual(named[0].host?.app, "/Applications/Hermes.app")
        XCTAssertNil(named[1].host?.app)
    }

    func testStaleAppScanCannotInventAConnectedSession() throws {
        let roster = AppRoster(
            apps: try apps(#"[{"target":"cursor","detected":true,"linked":true,"live":true,"live_sessions":1}]"#),
            sessions: []
        )
        XCTAssertTrue(roster.connected.isEmpty)
        XCTAssertEqual(roster.idle.map(\.target), ["cursor"])
    }

    func testClaudeCodeAndCodexSessionsStayWithTheirOwnApps() throws {
        let roster = AppRoster(
            apps: try apps(#"[{"target":"codex-cli","detected":true},{"target":"claude-code","detected":true}]"#),
            sessions: try sessions(#"[{"transport":"ipc","session_id":"codex-1","client_type":"Codex CLI","client_info":"codex-mcp-client","connected_secs":5},{"transport":"ipc","session_id":"claude-1","client_type":"Claude Code","client_info":"claude-code","connected_secs":9}]"#)
        )
        XCTAssertEqual(roster.connected[0].sessions.map(\.sessionId), ["codex-1"])
        XCTAssertEqual(roster.connected[1].sessions.map(\.sessionId), ["claude-1"])
        XCTAssertTrue(roster.other.isEmpty)
    }
}

@MainActor
final class PopoverRecentTests: XCTestCase {
    private func event(_ sequence: UInt64, tool: String?, server: String? = "Figma") -> ActivityEvent {
        ActivityEvent(
            sequence: sequence,
            occurredAtMs: sequence,
            client: nil,
            method: tool == nil ? "tools/list" : "tools/call",
            server: server,
            tool: tool,
            latencyMs: 10,
            outcome: "success"
        )
    }

    func testRecentCallsAreToolCallsNewestFirst() {
        let events = [
            event(1, tool: "Figma__get_file"),
            event(2, tool: nil),
            event(3, tool: "Slack__channels_list"),
            event(4, tool: ""),
            event(5, tool: "Notion__search"),
            event(6, tool: "Gmail__send_message"),
        ]
        XCTAssertEqual(PlugPopover.recentCalls(events, limit: 3).map(\.sequence), [6, 5, 3])
    }

    func testCallPartsSplitTheServerPrefix() {
        let parts = PlugPopover.callParts(event(1, tool: "Figma__get_file", server: "figma"))
        XCTAssertEqual(parts.server, "Figma")
        XCTAssertEqual(parts.tool, "get_file")
    }

    func testCallPartsKeepAnUnprefixedName() {
        let parts = PlugPopover.callParts(event(1, tool: "search", server: "notion"))
        XCTAssertEqual(parts.server, "notion")
        XCTAssertEqual(parts.tool, "search")
    }
}
