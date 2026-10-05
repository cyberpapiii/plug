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
            ServerForm.parseSettings("API_KEY=abc\nREGION=us-east-1"),
            ["API_KEY": "abc", "REGION": "us-east-1"]
        )
    }

    func testCommasStayInsideValues() {
        XCTAssertEqual(
            ServerForm.parseSettings("A=1, B=2"),
            ["A": "1, B=2"]
        )
    }

    func testSpaceAroundTheNameAndValueIsTrimmed() {
        XCTAssertEqual(ServerForm.parseSettings("  TOKEN = xyz  "), ["TOKEN": "xyz"])
    }

    func testValuesKeepTheirOwnEqualsSigns() {
        XCTAssertEqual(ServerForm.parseSettings("URL=a=b=c"), ["URL": "a=b=c"])
    }

    func testAnEmptyValueIsKept() {
        XCTAssertEqual(ServerForm.parseSettings("EMPTY="), ["EMPTY": ""])
    }

    func testLinesWithoutAnEqualsOrANameAreSkipped() {
        XCTAssertEqual(ServerForm.parseSettings("nonsense\n=value\n\nA=1"), ["A": "1"])
    }

    func testNothingTypedMeansNoEnvironment() {
        XCTAssertTrue(ServerForm.parseSettings("   \n ").isEmpty)
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
        XCTAssertTrue(AppModel.serverConfigReadRequiredCopy.contains("updating"))
        XCTAssertFalse(AppModel.serverConfigReadRequiredCopy.contains("PARSE_ERROR"))
        XCTAssertFalse(AppModel.serverConfigReadRequiredCopy.contains("could not be loaded"))
    }

    func testArgumentsRoundTripThroughTheDisplayedCommandLine() {
        let arguments = ["-y", "a package", "", "it's-safe", #"a\"quote"#]
        XCTAssertEqual(
            ServerDraftParser.tokenize(ServerForm.renderArguments(arguments)),
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
            ["Hermes", "ditto-history", "python3", "Cursor", "Unknown client abcd"]
        )
        XCTAssertEqual(named[0].host?.app, "/Applications/Hermes.app")
        XCTAssertNil(named[1].host?.app)
    }

    func testANameTheOwnerGaveWinsAndFollowsTheClientNotTheSession() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let key = "host:/opt/hermes_agent/bin/python3"
        let visibility = try decoder.decode(
            [ClientVisibility].self,
            from: Data(
                """
                [{"session_id":"h1","client_type":"Unknown","visible_tool_count":3,"client_key":"\(key)"},
                 {"session_id":"h2","client_type":"Unknown","visible_tool_count":3,"client_key":"\(key)"},
                 {"session_id":"h3","client_type":"Unknown","visible_tool_count":3}]
                """.utf8
            )
        )
        let stored = try decoder.decode(
            [ClientName].self,
            from: Data(#"[{"key":"\#(key)","name":"Hermes"},{"key":"oauth:abc","name":"Phone"}]"#.utf8)
        )
        let names = ClientNames(visibility: visibility, names: stored)
        let live = try sessions(
            """
            [{"transport":"daemon_proxy","session_id":"h1","client_type":"Unknown","client_info":"mcp","connected_secs":1,
              "host":{"name":"python3","executable":"/opt/hermes_agent/bin/python3"}},
             {"transport":"daemon_proxy","session_id":"h2","client_type":"Unknown","client_info":"mcp","connected_secs":1,
              "host":{"name":"python3","executable":"/opt/hermes_agent/bin/python3"}},
             {"transport":"daemon_proxy","session_id":"h3","client_type":"Unknown","client_info":"mcp","connected_secs":1}]
            """
        )

        XCTAssertEqual(live.map(names.displayName), ["Hermes", "Hermes", "Unknown client h3"])
        XCTAssertEqual(names.key(of: live[0]), key)
        XCTAssertNil(names.key(of: live[2]), "a session with nothing to store a name under cannot be renamed")
        XCTAssertEqual(names.name(forKey: "oauth:abc"), "Phone")
        XCTAssertNil(names.name(forKey: "oauth:other"))
    }

    func testAClientIsKeptFromWhatItsOwnKeyNamesAndNothingElse() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let servers = try decoder.decode(
            [ConfiguredServer].self,
            from: Data(
                """
                [{"name":"git","enabled":true,"transport":"stdio","oauth":false},
                 {"name":"slack","enabled":true,"transport":"http","oauth":true}]
                """.utf8
            )
        )
        let blocks = try decoder.decode(
            [ClientBlocks].self,
            from: Data(
                """
                [{"key":"cursor","servers":["git","gone"],"tools":["slack__post"]},
                 {"key":"oauth:abc","servers":["slack"]},
                 {"key":"pi","tools":["git__*","slack__dm"]}]
                """.utf8
            )
        )
        let access = { ClientAccess(key: $0, name: "Client", servers: servers, blocks: blocks) }

        // A block on a server that is gone is not counted.
        XCTAssertEqual(access("cursor").blockedServers, ["git"])
        XCTAssertEqual(access("cursor").summary, "1 server and 1 tool off")
        XCTAssertFalse(access("cursor").isRemote)
        XCTAssertEqual(access("oauth:abc").summary, "1 server off")
        XCTAssertTrue(access("oauth:abc").isRemote)
        XCTAssertEqual(access("pi").summary, "2 tools off")
        XCTAssertNil(access("claude-code").summary)
        XCTAssertFalse(access("claude-code").isLimited)

        // A tool is off by its own name, or under a rule that covers it.
        XCTAssertEqual(access("pi").state(ofTool: "Slack__DM"), .off)
        XCTAssertEqual(access("pi").state(ofTool: "Git__commit"), .offByRule("git__*"))
        XCTAssertEqual(access("pi").state(ofTool: "slack__post"), .on)
        XCTAssertEqual(access("cursor").state(ofTool: "slack__post"), .off)
        let tools = ["git__commit", "git__log", "slack__dm", "slack__post"].map {
            ToolFacts(name: $0, server: String($0.prefix(while: { $0 != "_" })))
        }
        XCTAssertEqual(access("pi").offCount(among: tools), 3)
        XCTAssertEqual(access("claude-code").offCount(among: tools), 0)
    }

    func testARuleFitsTheWayTheDaemonReadsIt() {
        XCTAssertTrue(ClientAccess.rule("*", fits: "anything"))
        XCTAssertTrue(ClientAccess.rule("git__*", fits: "git__log"))
        XCTAssertTrue(ClientAccess.rule("*__delete", fits: "git__delete"))
        XCTAssertTrue(ClientAccess.rule("git__*_all", fits: "git__push_all"))
        XCTAssertTrue(ClientAccess.rule("*delete*", fits: "git__delete_branch"))
        XCTAssertFalse(ClientAccess.rule("git__*", fits: "slack__git__log"))
        XCTAssertFalse(ClientAccess.rule("*__delete", fits: "git__delete_branch"))
        XCTAssertFalse(ClientAccess.rule("git__log", fits: "git__logs"))
    }

    func testARemoteSessionIsNamedAfterItsGrantUnlessPlugKnowsTheProduct() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let visibility = try decoder.decode(
            [ClientVisibility].self,
            from: Data(
                #"""
                [{"session_id":"r1","client_type":"Unknown","visible_tool_count":3,"client_key":"oauth:abc"},
                 {"session_id":"r2","client_type":"Cursor","visible_tool_count":3,"client_key":"oauth:def"},
                 {"session_id":"r3","client_type":"Unknown","visible_tool_count":3,"client_key":"oauth:gone"}]
                """#.utf8
            )
        )
        let grants = try decoder.decode(
            [DownstreamClient].self,
            from: Data(
                #"""
                [{"client_id":"abc","client_name":"Perplexity","redirect_uris":[],"source":"dynamic"},
                 {"client_id":"def","client_name":"Something Else","redirect_uris":[],"source":"dynamic"}]
                """#.utf8
            )
        )
        let live = try sessions(
            """
            [{"transport":"http","session_id":"r1","client_type":"Unknown","connected_secs":1},
             {"transport":"http","session_id":"r2","client_type":"Cursor","connected_secs":1},
             {"transport":"http","session_id":"r3","client_type":"Unknown","connected_secs":1}]
            """
        )

        let names = ClientNames(visibility: visibility, names: [], grants: grants)
        XCTAssertEqual(
            live.map(names.displayName),
            ["Perplexity", "Cursor", "Unknown client r3"]
        )
        XCTAssertEqual(names.key(of: live[0]), "oauth:abc", "a remote session is renamed through its grant")
        // The menu bar panel shows the same clients under the same names.
        XCTAssertEqual(
            names.connectedClients(live + live).map(\.name),
            ["Perplexity", "Cursor", "Unknown client r3"]
        )
        XCTAssertEqual(names.connectedClients(live)[1].target, "cursor")

        let renamed = ClientNames(
            visibility: visibility,
            names: try decoder.decode([ClientName].self, from: Data(#"[{"key":"oauth:def","name":"Work laptop"}]"#.utf8)),
            grants: grants
        )
        XCTAssertEqual(renamed.displayName(live[1]), "Work laptop")
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

    func testAClientRowSaysOneThingAboutItsState() throws {
        let list = try apps(#"""
        [{"target":"cursor","detected":true,"linked":true},
         {"target":"codex-cli","detected":true},
         {"target":"goose","linked":true}]
        """#)
        XCTAssertEqual(ClientStatus.app(list[0], connections: 2, limit: nil).text, "2 connections")
        XCTAssertEqual(ClientStatus.app(list[0], connections: 1, limit: "1 server off").text, "Connected · 1 server off")
        XCTAssertEqual(ClientStatus.app(list[0], connections: 0, limit: nil).text, "Not open")
        XCTAssertEqual(ClientStatus.app(list[1], connections: 0, limit: nil).text, "Not using Plug")
        XCTAssertEqual(ClientStatus.app(list[2], connections: 0, limit: nil).text, "Not found on this Mac")
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

    func testCallFactsSplitTheServerPrefix() {
        let call = CallFacts(event(1, tool: "Figma__get_file", server: "figma"))
        XCTAssertEqual(call.server, "Figma")
        XCTAssertEqual(call.tool, "get_file")
    }

    func testCallFactsKeepAnUnprefixedName() {
        let call = CallFacts(event(1, tool: "search", server: "notion"))
        XCTAssertEqual(call.server, "notion")
        XCTAssertEqual(call.tool, "search")
    }

    func testACallThatWorkedHasNoReasonOrAdvice() {
        let call = CallFacts(event(1, tool: "Figma__get_file"))
        XCTAssertEqual(call.result, "Worked")
        XCTAssertNil(call.reason)
        XCTAssertNil(call.advice)
        XCTAssertEqual(call.caller, "Unknown client")
    }

    func testAFailedCallSaysWhyAndWhatToDo() {
        let failed = CallFacts(ActivityEvent(
            sequence: 1, occurredAtMs: 1, client: nil, method: "tools/call", server: "notion",
            tool: "Notion__search", clientType: "claude-code", latencyMs: 30_000,
            outcome: "error", reason: "upstream request timed out"
        ))
        XCTAssertEqual(failed.result, "Failed")
        XCTAssertEqual(failed.caller, "Claude Code")
        XCTAssertEqual(failed.reason, "upstream request timed out")
        XCTAssertEqual(failed.advice, Explain.advice(forReason: "timed out"))
        XCTAssertEqual(failed.duration, "30.0 s")

        let old = CallFacts(ActivityEvent(
            sequence: 2, occurredAtMs: 1, client: nil, method: "tools/call", server: "notion",
            latencyMs: 12, outcome: "error"
        ))
        XCTAssertNil(old.reason)
        XCTAssertNotNil(old.advice, "a failure always has a next step")
        XCTAssertEqual(old.duration, "12 ms")
    }

    func testACancelledCallIsNotAFailure() {
        func call(_ outcome: String) -> CallFacts {
            CallFacts(ActivityEvent(
                sequence: 1, occurredAtMs: 1, client: nil, method: "tools/call", server: "notion",
                latencyMs: 12, outcome: outcome
            ))
        }
        let cancelled = call("cancelled")
        XCTAssertTrue(cancelled.cancelled)
        XCTAssertFalse(cancelled.failed)
        XCTAssertFalse(cancelled.succeeded)
        XCTAssertEqual(cancelled.result, "Canceled")
        XCTAssertEqual(cancelled.advice, "The client stopped this call before it finished.")

        let failed = call("error")
        XCTAssertTrue(failed.failed)
        XCTAssertFalse(failed.cancelled)
        XCTAssertEqual(failed.result, "Failed")
        XCTAssertEqual(failed.advice, "Try the call again to see why it fails.")

        let worked = CallFacts(event(1, tool: "Figma__get_file"))
        XCTAssertFalse(worked.failed)
        XCTAssertFalse(worked.cancelled)
    }

    func testEveryFailureGetsANextStep() {
        XCTAssertTrue(Explain.advice(forReason: "HTTP 401 Unauthorized").contains("Sign in"))
        XCTAssertTrue(Explain.advice(forReason: "connection refused").contains("address"))
        XCTAssertTrue(Explain.advice(forReason: "No such file or directory (os error 2)").contains("command"))
        XCTAssertEqual(Explain.advice(forReason: "took 1500 ms and broke"), Explain.fallback, "1500 is not a 500")
        XCTAssertEqual(Explain.advice(forReason: "zzz"), Explain.fallback)
    }

    func testAPartialImportNamesWhatFailed() {
        XCTAssertEqual(
            ImportServersView.summary(failed: ["notion"], of: 3),
            "Could not add notion. 2 servers were added. The reason is under each one."
        )
        XCTAssertEqual(
            ImportServersView.summary(failed: ["a", "b", "c", "d", "e"], of: 5),
            "Could not add a, b, c, and 2 more. The reason is under each one."
        )
    }
}
