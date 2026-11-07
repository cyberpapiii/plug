import XCTest
import PlugIPC
@testable import Plug

/// The tool list is the surface where someone switches a single capability off,
/// so these pin what a search finds, how a name reads, and which tools are
/// honestly out of reach because a wildcard covers them.
final class ToolCatalogTests: XCTestCase {
    private let catalog = ToolCatalog([
        ToolFacts(delimiter: "__", name: "figma__get_file", server: "figma", summary: "Read a design file"),
        ToolFacts(delimiter: "__", name: "figma__export_frame", server: "figma", summary: "Export an image"),
        ToolFacts(
            delimiter: "__",
            name: "notion__search",
            server: "notion",
            summary: "Search pages",
            isOn: false
        ),
        ToolFacts(
            delimiter: "__",
            name: "notion__append_block",
            server: "notion",
            isOn: false,
            lockedByPattern: "notion__append*"
        ),
    ])

    func testShortNameDropsTheServerPrefixTheGroupAlreadyStates() {
        XCTAssertEqual(
            ToolFacts(delimiter: "__", name: "figma__get_file", server: "figma").shortName,
            "get_file"
        )
    }

    func testShortNameKeepsNamesThatDoNotCarryThePrefix() {
        XCTAssertEqual(ToolFacts(delimiter: "__", name: "search", server: "notion").shortName, "search")
    }

    func testShortNameMatchesThePrefixRegardlessOfCase() {
        XCTAssertEqual(
            ToolFacts(delimiter: "__", name: "Figma__Get_File", server: "figma").shortName,
            "Get_File"
        )
    }

    func testAServersToolsAreAlphabetical() {
        XCTAssertEqual(catalog.tools(for: "figma").map(\.shortName), ["export_frame", "get_file"])
    }

    func testSearchingMatchesDescriptionsNotJustNames() {
        XCTAssertEqual(
            catalog.tools(for: "figma", matching: "export an image").map(\.name),
            ["figma__export_frame"]
        )
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
        XCTAssertEqual(catalog.servers(withToolsMatching: "  EXPORT "), ["figma"])
        XCTAssertEqual(catalog.tools(for: "figma", matching: "  EXPORT ").map(\.shortName), ["export_frame"])
    }

    func testAToolCoveredByAWildcardNamesIt() {
        let covered = catalog.tools(for: "notion").first { $0.lockedByPattern != nil }
        XCTAssertEqual(covered?.lockedByPattern, "notion__append*")
    }

    func testDaemonToolsBecomeFactsWithoutLosingWhySomethingIsOff() {
        let facts = ToolFacts(
            ToolInfo(
                name: "notion__append_block",
                serverId: "notion",
                title: "Append block",
                disabled: true,
                disabledByPattern: "notion__append*"
            ),
            delimiter: "__"
        )
        XCTAssertEqual(facts.server, "notion")
        XCTAssertEqual(facts.summary, "Append block")
        XCTAssertFalse(facts.isOn)
        XCTAssertEqual(facts.lockedByPattern, "notion__append*")
    }

    func testDescriptionIsPreferredOverTitleWhenBothArrive() {
        let facts = ToolFacts(
            ToolInfo(name: "figma__get_file", serverId: "figma", description: "Read a design file", title: "Get file"),
            delimiter: "__"
        )
        XCTAssertEqual(facts.summary, "Read a design file")
        XCTAssertTrue(facts.isOn)
        XCTAssertNil(facts.lockedByPattern)
    }

    func testPatternListsEveryToolItCovers() {
        let catalog = ToolCatalog([
            ToolFacts(delimiter: "__", name: "notion__search", server: "notion", isOn: false, lockedByPattern: "notion__*"),
            ToolFacts(delimiter: "__", name: "notion__append", server: "notion", isOn: false, lockedByPattern: "notion__*"),
            ToolFacts(delimiter: "__", name: "figma__get_file", server: "figma"),
            ToolFacts(delimiter: "__", name: "figma__delete", server: "figma", isOn: false),
        ])
        XCTAssertEqual(
            catalog.tools(coveredBy: "notion__*").map(\.shortName),
            ["append", "search"]
        )
        XCTAssertTrue(catalog.tools(coveredBy: "figma__*").isEmpty)
    }
}

/// The app list comes from `plug clients --output json`; `CLIReportTests`
/// reads what that command prints. These cover the fields a listing may
/// leave out.
final class LinkableAppTests: XCTestCase {
    private func decode(_ json: String) throws -> [LinkableApp] {
        struct Listing: Decodable { let clients: [LinkableApp] }
        return try JSONDecoder().decode(Listing.self, from: Data(json.utf8)).clients
    }

    func testAddAClientCoversEveryClientAndBothWaysIn() throws {
        let apps = try decode(
            """
            {"clients":[
            {"target":"zed","name":"Zed","linked":false,"detected":true},
            {"target":"cursor","name":"Cursor","linked":true,"detected":true,"linked_transport":"http"},
            {"target":"amp","name":"Amp","linked":false,"detected":false},
            {"target":"claude-code","name":"Claude Code","linked":true,"detected":true,"linked_transport":"stdio"}]}
            """
        )
        let remote = "https://plug.example.com/mcp"
        func step(_ choice: AddClient.Choice, _ way: AddClient.Way, address: String? = remote) -> AddClient.Step {
            AddClient.step(choice, way: way, apps: apps, address: address)
        }
        XCTAssertEqual(AddClient.found(apps).map(\.target), ["claude-code", "cursor", "zed"])
        XCTAssertEqual(AddClient.notFound(apps).map(\.target), ["amp"])
        XCTAssertEqual(AddClient.firstChoice(apps), .app("zed"))

        // A client on this Mac is one button, either way in.
        XCTAssertEqual(step(.app("zed"), .onThisMac), .write(target: "zed"))
        XCTAssertEqual(step(.app("zed"), .network), .write(target: "zed"))
        // One already in says so, and can still be moved to the other way.
        XCTAssertEqual(step(.app("cursor"), .network), .done(target: "cursor"))
        XCTAssertEqual(step(.app("cursor"), .onThisMac), .write(target: "cursor"))
        XCTAssertEqual(step(.app("claude-code"), .onThisMac), .done(target: "claude-code"))
        // Plug writes nothing for a client it cannot find or does not know.
        XCTAssertEqual(step(.app("amp"), .onThisMac), .command)
        XCTAssertEqual(step(.other, .onThisMac), .command)
        XCTAssertEqual(step(.other, .network), .address)
        // A client on the web needs an address it can reach.
        XCTAssertEqual(AddClient.ways(for: .hosted("ChatGPT")), [.network])
        XCTAssertEqual(step(.hosted("ChatGPT"), .network), .address)
        XCTAssertEqual(step(.hosted("ChatGPT"), .network, address: "http://localhost:3282/mcp"), .unreachable)
        XCTAssertEqual(step(.hosted("ChatGPT"), .network, address: nil), .unreachable)
    }

    func testWhatIsPastedIsAnEntryAClientAccepts() throws {
        XCTAssertTrue(AddClient.reachesBeyondThisMac("https://plug.example.com/mcp"))
        XCTAssertFalse(AddClient.reachesBeyondThisMac("http://127.0.0.1:3282/mcp"))
        XCTAssertTrue(AddClient.addressAdvice("http://localhost:3282/mcp", name: nil).contains("only on this Mac"))
        XCTAssertTrue(AddClient.addressAdvice("https://plug.example.com/mcp", name: "ChatGPT").hasPrefix("In ChatGPT,"))

        func plug(_ entry: String) throws -> [String: Any] {
            let object = try JSONSerialization.jsonObject(with: Data(entry.utf8)) as? [String: Any]
            let servers = object?["mcpServers"] as? [String: Any]
            return try XCTUnwrap(servers?["plug"] as? [String: Any])
        }
        let local = try plug(AddClient.settingsEntry(command: "/Applications/Plug.app/Contents/Resources/plug"))
        XCTAssertEqual(local["command"] as? String, "/Applications/Plug.app/Contents/Resources/plug")
        XCTAssertEqual(local["args"] as? [String], ["connect"])

        // A client the owner names connects under a key made from the name.
        XCTAssertEqual(AddClient.customTarget(named: "  My Tool (v2)! "), "custom:my-tool-v2")
        XCTAssertNil(AddClient.customTarget(named: " ?? "))
        // Each made-up client keeps its own icon, apart from any other's.
        XCTAssertNotEqual(
            IconStore.key(client: "custom:my-tool-v2"),
            IconStore.key(client: "custom:my-tool")
        )
        XCTAssertEqual(AddClient.customTarget(named: String(repeating: "a", count: 60))?.count, 47)
        let named = try plug(AddClient.settingsEntry(command: "/plug", target: "custom:my-tool"))
        XCTAssertEqual(named["args"] as? [String], ["connect", "--client", "custom:my-tool"])
        XCTAssertEqual(
            try plug(AddClient.settingsEntry(address: "https://plug.example.com/mcp"))["url"] as? String,
            "https://plug.example.com/mcp"
        )
    }

    func testANetworkClientWithNoSignInLeftSaysSoAndHow() throws {
        XCTAssertEqual(ClientStatus.grant(connections: 0, needsSignIn: true, limit: nil).text, "Needs sign-in")
        XCTAssertEqual(
            ClientStatus.grant(connections: 0, needsSignIn: false, limit: nil).text, "Allowed over the network"
        )
        // An open connection is proof of a sign-in.
        XCTAssertEqual(ClientStatus.grant(connections: 1, needsSignIn: true, limit: nil).text, "Connected")
        XCTAssertTrue(ClientSignIn.steps(target: "codex-cli", name: "Codex CLI").contains("codex mcp login plug"))
        XCTAssertEqual(ClientSignIn.steps(target: "devin", name: "Devin"), "Open Devin and use Plug from there.")
        // Plug offers a Sign In button only where the client has a command.
        XCTAssertEqual(ClientSignIn.command(target: "codex-cli"), "codex mcp login plug")
        XCTAssertNil(ClientSignIn.command(target: "claude-code"))
        XCTAssertTrue(ClientSignIn.about(target: "codex-cli", name: "Codex CLI").contains("Press Sign In."))

        let decoder = JSONDecoder()
        let lapsed = try decoder.decode(
            DownstreamClient.self,
            from: Data(#"{"clientId":"a","clientName":"A","redirectUris":[],"source":"test","clientKey":"oauth:a","signedIn":false,"createdAt":1,"expiresAt":2}"#.utf8)
        )
        XCTAssertTrue(lapsed.needsSignIn)
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
                [{"transport":"daemon_proxy","session_id":"a1","client_type":"CodexCli","target":"codex-cli","connected_secs":5},
                 {"transport":"daemon_proxy","session_id":"a2","client_type":"CodexCli","target":"codex-cli","connected_secs":9}]
                """
            )
        )
        XCTAssertEqual(roster.connected.map(\.app.target), ["codex-cli"])
        XCTAssertEqual(roster.connected[0].sessions.map(\.sessionId), ["a1", "a2"])
        XCTAssertEqual(roster.idle.map(\.target), ["cursor"])
        XCTAssertTrue(roster.other.isEmpty)
    }

    func testASessionItsOwnerNamedIsNotTheClientItReports() throws {
        let live = try sessions(
            """
            [{"transport":"http","session_id":"g1","client_type":"Cursor","target":"cursor",
              "client_key":"cursor","access_key":"remote:shared"},
             {"transport":"http","session_id":"c1","client_type":"Cursor","target":"cursor",
              "client_key":"cursor","access_key":"remote:shared"}]
            """
        )
        let roster = AppRoster(
            apps: try apps(#"[{"target":"cursor","linked":true,"detected":true,"linked_transport":"http"}]"#),
            sessions: live
        ) { $0.sessionId == "g1" ? "grok-bot" : nil }
        XCTAssertEqual(roster.connected[0].sessions.map(\.sessionId), ["c1"])
        XCTAssertEqual(roster.other.map(\.sessionId), ["g1"])
    }

    /// Cursor linked on this Mac, and a Cursor on another Mac signed in over
    /// the network. The daemon gives both the target `cursor`, so only how
    /// each reaches Plug tells them apart.
    func testASessionOverTheNetworkIsNotTheLocalAppsThoughItHasItsTarget() throws {
        let live = try sessions(
            """
            [{"transport":"daemon_proxy","session_id":"l1","client_type":"Cursor","target":"cursor",
              "client_key":"cursor","access_key":"cursor"},
             {"transport":"http","session_id":"r1","client_type":"Cursor","target":"cursor",
              "client_key":"oauth:abc","access_key":"oauth:abc"},
             {"transport":"http","session_id":"r2","client_type":"Cursor","target":"cursor",
              "client_key":"cursor","access_key":"remote:shared"}]
            """
        )
        let local = try apps(#"[{"target":"cursor","linked":true,"detected":true,"linked_transport":"stdio"}]"#)
        let roster = AppRoster(apps: local, sessions: live)
        XCTAssertEqual(roster.connected.map(\.app.target), ["cursor"])
        XCTAssertEqual(roster.connected[0].sessions.map(\.sessionId), ["l1"])
        XCTAssertEqual(roster.other.map(\.sessionId), ["r1", "r2"])

        // Linked over the network, the app holds the session with no sign-in
        // of its own. The signed-in one is still its sign-in's.
        let remote = try apps(#"[{"target":"cursor","linked":true,"detected":true,"linked_transport":"http"}]"#)
        let overNetwork = AppRoster(apps: remote, sessions: live)
        XCTAssertEqual(overNetwork.connected[0].sessions.map(\.sessionId), ["l1", "r2"])
        XCTAssertEqual(overNetwork.other.map(\.sessionId), ["r1"])

        // The window and the menu bar count the same three clients.
        let names = ClientNames(names: [])
        XCTAssertEqual(live.map(names.client(of:)), ["cursor", "oauth:abc", "remote:cursor"])
        XCTAssertEqual(names.connectedClients(live + live).count, 3)
    }

    func testASessionFromNoKnownAppIsKeptApart() throws {
        let roster = AppRoster(
            apps: try apps(#"[{"target":"cursor","detected":true}]"#),
            sessions: try sessions(
                #"[{"transport":"daemon_proxy","session_id":"z9","client_type":"Unknown","client_info":"ditto-history","connected_secs":1}]"#
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
             {"transport":"daemon_proxy","session_id":"h4","client_type":"Cursor","client_info":"cursor-vscode","target":"cursor","connected_secs":1,
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

    func testAServersToolGroupsAreReadFromItsToolNames() {
        XCTAssertEqual(
            ToolCatalog.groups(ofNames: [
                "Gmail__search_messages", "GoogleDrive__list_items", "Gmail__send_message", "Workspace__start",
            ], delimiter: "__"),
            ["Gmail", "GoogleDrive", "Workspace"]
        )
        XCTAssertEqual(ToolCatalog.groups(ofNames: ["Slack__channels_list", "Slack__users_search"], delimiter: "__"), ["Slack"])
        XCTAssertEqual(ToolCatalog.groups(ofNames: ["unprefixed"], delimiter: "__"), [])
    }

    func testClientsAreGroupedByWhereTheyRun() throws {
        // A client sits under how it reaches Plug until the owner says
        // where it runs. One that does not use Plug has no place.
        XCTAssertEqual(ClientPlaces.place(owners: nil, group: .onThisMac), "On This Mac")
        XCTAssertEqual(ClientPlaces.place(owners: "", group: .network), "Over the Network")
        XCTAssertEqual(ClientPlaces.place(owners: "Work laptop", group: .network), "Work laptop")
        XCTAssertEqual(ClientPlaces.place(owners: "Work laptop", group: .notUsing), "No Local Plug Configuration")

        XCTAssertEqual(
            ClientPlaces.ordered([
                "No Local Plug Configuration", "Work laptop", "Over the Network", "Cloud", "On This Mac", "Cloud",
            ]),
            ["On This Mac", "Cloud", "Work laptop", "Over the Network", "No Local Plug Configuration"]
        )
        XCTAssertEqual(ClientPlaces.ordered(["Cloud"]), ["Cloud"])

        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let places = try decoder.decode(
            [ClientPlace].self,
            from: Data(#"[{"key":"oauth:abc","place":"Work laptop"},{"key":"oauth:def","place":"Cloud"}]"#.utf8)
        )
        // The daemon's own reply, read the way the app reads it.
        let snapshot = try decoder.decode(OperatorSnapshot.self, from: Data(#"""
        {"runtime_version":"1","uptime_secs":1,"tool_catalog_revision":0,"ownership":"app_managed","prefix_delimiter":"__","configured_servers":[],
         "servers":[],"live_sessions":[],"upstream_auth":[],"downstream_clients":[],
         "client_places":[{"key":"claude-code","place":"Work laptop"}]}
        """#.utf8))
        XCTAssertEqual(ClientNames(snapshot: snapshot).place(forKey: "claude-code"), "Work laptop")

        let names = ClientNames(names: [], places: places)
        XCTAssertEqual(names.place(forKey: "oauth:abc"), "Work laptop")
        XCTAssertNil(names.place(forKey: "oauth:zzz"))
        XCTAssertEqual(names.ownerPlaces, ["Cloud", "Work laptop"])
    }

    func testAServersSettingsPlaceIsOnlyWhatPlugAlreadyKnows() {
        let home = "/Users/someone"
        let onDisk: Set<String> = [
            "/Applications/Figma.app", "/Users/someone/tools/server.py", "/Users/someone/notes/config.toml",
        ]
        func place(_ config: ServerConfig, website: String? = nil) -> ServerSettingsPlace {
            ServerSettingsPlace.find(config: config, website: website, home: home, exists: onDisk.contains)
        }
        // The app a command runs from comes first.
        let fromApp = place(.command("/Applications/Figma.app/Contents/MacOS/mcp", args: ["~/tools/server.py"]))
        XCTAssertEqual(fromApp, .app(path: "/Applications/Figma.app"))
        XCTAssertEqual(fromApp.label(home: home), "In Figma")
        XCTAssertEqual(fromApp.actionTitle, "Open Figma")
        // Then a file its arguments name, bare or behind an option.
        let fromFile = place(.command("uv", args: ["run", "~/tools/server.py"]), website: "https://example.com")
        XCTAssertEqual(fromFile, .file(path: "/Users/someone/tools/server.py"))
        XCTAssertEqual(fromFile.label(home: home), "~/tools/server.py")
        XCTAssertEqual(
            place(.command("node", args: ["--config=/Users/someone/notes/config.toml"])),
            .file(path: "/Users/someone/notes/config.toml")
        )
        // A path that is not there, or an app that is gone, is not offered.
        XCTAssertEqual(place(.command("/Applications/Gone.app/Contents/MacOS/x", args: ["/nowhere/x.py"])), .plug)
        // Then the page the server gave, when it is a safe one.
        XCTAssertEqual(
            place(.remote("https://mcp.example.com/mcp"), website: "https://example.com/docs"),
            .page(URL(string: "https://example.com/docs")!)
        )
        XCTAssertEqual(place(.remote("https://mcp.example.com/mcp"), website: "http://example.com"), .plug)
        XCTAssertEqual(place(.command("npx", args: ["-y", "some-server"])).actionTitle, "Edit…")
    }

    func testAClientNamedAsOnePlugKnowsTakesThatClientsIcon() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let stored = try decoder.decode(
            [ClientName].self,
            from: Data(#"[{"key":"oauth:abc","name":"GrokBot"},{"key":"oauth:def","name":"Work laptop"}]"#.utf8)
        )
        let names = ClientNames(names: stored)
        let live = try sessions(
            """
            [{"transport":"http","session_id":"g1","client_type":"Cursor","client_info":"Cursor","target":"cursor",
              "client_key":"oauth:abc","access_key":"oauth:abc"},
             {"transport":"http","session_id":"c1","client_type":"Cursor","client_info":"Cursor","target":"cursor",
              "client_key":"oauth:def","access_key":"oauth:def"},
             {"transport":"daemon_proxy","session_id":"m1","client_type":"Cursor","client_info":"Cursor","connected_secs":1,
              "client_key":"custom:my-tool","access_key":"custom:my-tool","target":"custom:my-tool"}]
            """
        )
        // The daemon's target wins over what the client reports.
        XCTAssertEqual(names.target(of: live[2]), "custom:my-tool")
        XCTAssertEqual(names.target(of: live[0]), "grok-bot")
        // A name Plug does not know leaves the icon to what the client reports.
        XCTAssertEqual(names.target(of: live[1]), "cursor")
        XCTAssertEqual(names.knownTarget(named: "GrokBot"), "grok-bot")
        XCTAssertEqual(names.knownTarget(named: "ChatGPT Desktop"), "chatgpt")
        XCTAssertNil(names.knownTarget(named: "Work laptop"))
    }

    func testANameTheOwnerGaveWinsAndFollowsTheClientNotTheSession() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let key = "host:/opt/hermes_agent/bin/python3"
        let stored = try decoder.decode(
            [ClientName].self,
            from: Data(#"[{"key":"\#(key)","name":"Hermes"},{"key":"oauth:abc","name":"Phone"}]"#.utf8)
        )
        let names = ClientNames(names: stored)
        let live = try sessions(
            """
            [{"transport":"daemon_proxy","session_id":"h1","client_type":"Unknown","client_info":"mcp","connected_secs":1,
              "host":{"name":"python3","executable":"/opt/hermes_agent/bin/python3"},
              "client_key":"\(key)","access_key":"\(key)"},
             {"transport":"daemon_proxy","session_id":"h2","client_type":"Unknown","client_info":"mcp","connected_secs":1,
              "host":{"name":"python3","executable":"/opt/hermes_agent/bin/python3"},
              "client_key":"\(key)","access_key":"\(key)"},
             {"transport":"daemon_proxy","session_id":"h3","client_type":"Unknown","client_info":"mcp","connected_secs":1}]
            """
        )

        XCTAssertEqual(live.map(names.displayName), ["Hermes", "Hermes", "Unknown client h3"])
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
        // What the daemon enforces of each client's lists.
        let blocks = try decoder.decode(
            [ClientBlocks].self,
            from: Data(
                """
                [{"key":"cursor","servers":["git","gone"],"tools":["slack__post"],
                  "off_servers":["git"],"off_tools":[{"name":"slack__post"}]},
                 {"key":"oauth:abc","servers":["slack"],"off_servers":["slack"]},
                 {"key":"pi","tools":["git__*","slack__dm"],
                  "off_tools":[{"name":"git__commit","rule":"git__*"},{"name":"git__log","rule":"git__*"},
                               {"name":"slack__dm"}]}]
                """.utf8
            )
        )
        let access = { ClientAccess(key: $0, name: "Client", isRemote: $0.hasPrefix("oauth"), servers: servers, blocks: blocks) }

        XCTAssertEqual(access("cursor").offServers, ["git"])
        XCTAssertEqual(access("cursor").summary, "1 server and 1 tool off")
        XCTAssertFalse(access("cursor").isOn(server: "git"))
        XCTAssertEqual(access("oauth:abc").summary, "1 server off")
        // Every tool a rule covers counts.
        XCTAssertEqual(access("pi").summary, "3 tools off")
        XCTAssertNil(access("claude-code").summary)
        XCTAssertFalse(access("claude-code").isLimited)

        // A tool is off by its own name, or under a rule that covers it.
        XCTAssertEqual(access("pi").state(ofTool: "Slack__DM"), .off)
        XCTAssertEqual(access("pi").state(ofTool: "Git__commit"), .offByRule("git__*"))
        XCTAssertEqual(access("pi").state(ofTool: "slack__post"), .on)
        XCTAssertEqual(access("cursor").state(ofTool: "slack__post"), .off)
        let tools = ["git__commit", "git__log", "slack__dm", "slack__post"].map {
            ToolFacts(delimiter: "__", name: $0, server: String($0.prefix(while: { $0 != "_" })))
        }
        XCTAssertEqual(access("pi").offCount(among: tools), 3)
        XCTAssertEqual(access("claude-code").offCount(among: tools), 0)
    }

    func testAClientWithAnAllowListSwitchesToolsOnTheList() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let servers = try decoder.decode(
            [ConfiguredServer].self,
            from: Data(
                """
                [{"name":"git","enabled":true,"transport":"stdio","oauth":false},
                 {"name":"slack","enabled":true,"transport":"http","oauth":true},
                 {"name":"notion","enabled":true,"transport":"http","oauth":true}]
                """.utf8
            )
        )
        let blocks = try decoder.decode(
            [ClientBlocks].self,
            from: Data(
                """
                [{"key":"radar","only_allowed":true,"allowed_servers":["slack"],
                  "allowed_tools":["Git__commit"],"tools":["slack__dm"],
                  "off_servers":["notion"],"off_tools":[{"name":"slack__dm"},{"name":"git__push"}]}]
                """.utf8
            )
        )
        let radar = ClientAccess(key: "radar", name: "Radar", servers: servers, blocks: blocks)
        XCTAssertTrue(radar.isOn(server: "git"))
        XCTAssertFalse(radar.isOn(server: "notion"))
        XCTAssertEqual(radar.summary, "1 server and 2 tools off")

        // On a server that is on the list whole, a tool's switch is a block.
        // On one that is not, it puts the tool on the list or takes it off.
        let tool = { ToolFacts(delimiter: "__", name: $0, server: $1) }
        XCTAssertEqual(radar.state(of: tool("slack__post", "slack")), .on)
        XCTAssertEqual(radar.state(of: tool("slack__dm", "slack")), .off)
        XCTAssertFalse(radar.isListedSingly(tool("slack__dm", "slack")))
        XCTAssertEqual(radar.state(of: tool("git__commit", "git")), .on)
        XCTAssertEqual(radar.state(of: tool("git__push", "git")), .off)
        XCTAssertTrue(radar.isListedSingly(tool("git__push", "git")))
    }

    func testARemoteSessionIsNamedAfterItsGrantUnlessPlugKnowsTheProduct() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let grants = try decoder.decode(
            [DownstreamClient].self,
            from: Data(
                #"""
                [{"client_id":"abc","client_name":"Perplexity","redirect_uris":[],"source":"dynamic","client_key":"oauth:abc","signed_in":true,"created_at":1,"expires_at":2},
                 {"client_id":"def","client_name":"Something Else","redirect_uris":[],"source":"dynamic","client_key":"oauth:def","signed_in":true,"created_at":1,"expires_at":2}]
                """#.utf8
            )
        )
        let live = try sessions(
            """
            [{"transport":"http","session_id":"r1","client_type":"Unknown","connected_secs":1,
              "client_key":"oauth:abc","access_key":"oauth:abc"},
             {"transport":"http","session_id":"r2","client_type":"Cursor","target":"cursor",
              "client_key":"oauth:def","access_key":"oauth:def"},
             {"transport":"http","session_id":"r3","client_type":"Unknown","connected_secs":1,
              "client_key":"oauth:gone","access_key":"oauth:gone"}]
            """
        )

        let names = ClientNames(names: [], grants: grants)
        XCTAssertEqual(
            live.map(names.displayName),
            ["Perplexity", "Cursor", "Unknown client r3"]
        )
        // The menu bar panel shows the same clients under the same names.
        XCTAssertEqual(
            names.connectedClients(live + live).map(\.name),
            ["Perplexity", "Cursor", "Unknown client r3"]
        )
        XCTAssertEqual(names.connectedClients(live)[1].target, "cursor")
        // One that reports nothing useful is pictured by its sign-in's name,
        // as the Clients list and a call picture it.
        XCTAssertEqual(names.connectedClients(live)[0].target, "perplexity")

        let renamed = ClientNames(
            names: try decoder.decode([ClientName].self, from: Data(#"[{"key":"oauth:def","name":"Work laptop"}]"#.utf8)),
            grants: grants
        )
        XCTAssertEqual(renamed.displayName(live[1]), "Work laptop")
    }

    func testOnlyASessionWhoseRequestsCarryItsOwnKeyHasChoicesOfItsOwn() throws {
        let live = try sessions(
            """
            [{"transport":"daemon_proxy","session_id":"l1","client_type":"Cursor","target":"cursor",
              "client_key":"cursor","access_key":"cursor"},
             {"transport":"http","session_id":"r1","client_type":"Cursor","target":"cursor",
              "client_key":"cursor","access_key":"remote:shared"},
             {"transport":"http","session_id":"r2","client_type":"Unknown","connected_secs":1,
              "access_key":"remote:shared"}]
            """
        )
        XCTAssertEqual(live.map(\.ownAccessKey), ["cursor", nil, nil])
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
            sessions: try sessions(#"[{"transport":"daemon_proxy","session_id":"codex-1","client_type":"CodexCli","client_info":"codex-mcp-client","target":"codex-cli","connected_secs":5},{"transport":"daemon_proxy","session_id":"claude-1","client_type":"ClaudeCode","client_info":"claude-code","target":"claude-code","connected_secs":9}]"#)
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
        XCTAssertEqual(ClientStatus.app(list[1], connections: 0, limit: nil).text, "No local Plug configuration")
        XCTAssertEqual(ClientStatus.app(list[2], connections: 0, limit: nil).text, "Not found on this Mac")
    }

    func testOnlyLinkedHTTPClientsAreRepresentedByTheirOwnGrant() throws {
        let list = try apps(#"""
        [{"target":"cursor","detected":true,"linked":true,"linked_transport":"http"},
         {"target":"claude-code","detected":true,"linked":true,"linked_transport":"stdio"},
         {"target":"codex-cli","detected":true,"linked_transport":"http"}]
        """#)
        XCTAssertTrue(ClientStatus.hasNetworkRepresentation(list[0], grantTargets: ["cursor"]))
        XCTAssertFalse(ClientStatus.hasNetworkRepresentation(list[0], grantTargets: ["claude-code"]))
        XCTAssertFalse(ClientStatus.hasNetworkRepresentation(list[1], grantTargets: ["claude-code"]))
        XCTAssertFalse(ClientStatus.hasNetworkRepresentation(list[2], grantTargets: ["codex-cli"]))
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
            outcome: .success
        )
    }

    func testACallIsShownUnderTheNameItsOwnerGaveTheClient() {
        let reported = ActivityEvent(
            sequence: 1, occurredAtMs: 0, client: "s1", method: "tools/call", server: "imessage",
            tool: "IMessage__get_messages", clientType: "Cursor", clientLabel: "Cursor 1.0",
            clientKey: "oauth:abc", target: "cursor", latencyMs: 1, outcome: .success
        )
        let named = CallFacts(reported, ownerName: "GrokBot")
        XCTAssertEqual(named.caller, "GrokBot")
        XCTAssertEqual(named.callerTarget, "grok-bot")
        // A name Plug does not know keeps the icon of what the client reports.
        // Where it runs follows the name, when the owner has said.
        XCTAssertEqual(
            CallFacts(reported, ownerName: "GrokBot", place: "Cloud").callerAndPlace, "GrokBot (Cloud)"
        )
        XCTAssertEqual(CallFacts(reported, ownerName: "GrokBot").callerAndPlace, "GrokBot")
        let other = CallFacts(reported, ownerName: "Work laptop")
        XCTAssertEqual(other.caller, "Work laptop")
        XCTAssertEqual(other.callerTarget, "cursor")
        XCTAssertEqual(CallFacts(reported).caller, "Cursor")
    }

    /// Activity's search finds a call by the names its row shows, not only
    /// by what the client reported.
    func testASearchFindsACallByTheNameOnItsRow() {
        let reported = ActivityEvent(
            sequence: 1, occurredAtMs: 0, client: "s1", method: "tools/call", server: "workspace",
            tool: "Gmail__send_message", clientType: "Claude Desktop", clientLabel: "claude-ai",
            clientKey: "claude-desktop", target: "claude-desktop", latencyMs: 1, outcome: .success
        )
        let call = CallFacts(reported, delimiter: "__")
        XCTAssertEqual(call.caller, "Claude Desktop")
        XCTAssertTrue(call.matches("claude desktop"))
        XCTAssertTrue(call.matches("claude-ai"))
        XCTAssertTrue(call.matches("workspace"))
        XCTAssertTrue(call.matches("gmail"))
        XCTAssertFalse(call.matches("figma"))
        let named = CallFacts(reported, delimiter: "__", ownerName: "Desk", place: "Studio")
        XCTAssertTrue(named.matches("desk"))
        XCTAssertTrue(named.matches("studio"))
    }

    func testAClientThatCameAndWentIsListedByItsLastCall() {
        func call(_ sequence: UInt64, at ms: UInt64, key: String?) -> ActivityEvent {
            ActivityEvent(
                sequence: sequence, occurredAtMs: ms, client: "s", method: "tools/call", server: "gmail",
                tool: "Gmail__search_messages", clientType: "Unknown", clientLabel: "radar",
                clientKey: key, latencyMs: 1, outcome: .success
            )
        }
        let events = [
            call(1, at: 100, key: "host:python#radar.py"),
            call(2, at: 300, key: "host:python#radar.py"),
            call(3, at: 200, key: "oauth:abc"),
            call(4, at: 400, key: "cursor"),
            call(5, at: 500, key: nil),
        ]
        // One row a client, by its newest call; a client already shown and a
        // call nobody can be told apart by add none.
        XCTAssertEqual(RecentCallers.latest(events, excluding: ["cursor"]).map(\.sequence), [2, 3])

        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = .gmt
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        XCTAssertTrue(RecentCallers.lastUsed(now.addingTimeInterval(-60), now: now, calendar: calendar).hasPrefix("Last used "))
        XCTAssertFalse(RecentCallers.lastUsed(now.addingTimeInterval(-60), now: now, calendar: calendar).contains(","))
        XCTAssertTrue(RecentCallers.lastUsed(now.addingTimeInterval(-3 * 86400), now: now, calendar: calendar).contains(","))
    }

    func testACallFromANetworkClientIsPicturedByItsSignIn() {
        func call(_ type: String, target: String? = nil, grant: String?) -> CallFacts {
            CallFacts(ActivityEvent(
                sequence: 1, occurredAtMs: 0, client: "s1", method: "tools/call", server: "imessage",
                tool: "IMessage__get_messages", clientType: type, clientKey: "oauth:abc",
                target: target, latencyMs: 1, outcome: .success
            ), grantName: grant)
        }
        // A client that says nothing useful about itself takes its sign-in's name.
        let hermes = call("Unknown", grant: "Hermes Agent")
        XCTAssertEqual(hermes.caller, "Hermes Agent")
        XCTAssertEqual(hermes.callerTarget, "hermes")
        let script = call("Unknown", grant: "Ditto email-manager")
        XCTAssertEqual(script.caller, "Ditto email-manager")
        // What a known client reports still outranks its sign-in.
        XCTAssertEqual(call("Cursor", target: "cursor", grant: "Something").callerTarget, "cursor")
        XCTAssertEqual(call("Unknown", grant: nil).caller, "Unknown client")
    }

    func testCallFactsSplitTheServerPrefix() {
        let call = CallFacts(event(1, tool: "Figma__get_file", server: "figma"), delimiter: "__")
        XCTAssertEqual(call.server, "figma")
        XCTAssertEqual(call.tool, "get_file")
    }

    /// A group rule files a server's tools under other names. The call is
    /// the server's that the daemon routed it to, and the tool keeps the
    /// name the tool list gives it.
    func testACallToAGroupsToolBelongsToTheServerThatAnswered() {
        let call = CallFacts(event(1, tool: "Gmail__send_message", server: "workspace"), delimiter: "__")
        XCTAssertEqual(call.server, "workspace")
        XCTAssertEqual(call.tool, "Gmail__send_message")
        XCTAssertEqual(
            call.tool, ToolFacts(delimiter: "__", name: "Gmail__send_message", server: "workspace").shortName
        )
    }

    /// The delimiter is the daemon's, whatever the config sets it to.
    func testCallFactsSplitOnTheDaemonsDelimiter() {
        let dotted = CallFacts(event(1, tool: "Figma.get_file", server: "figma"), delimiter: ".")
        XCTAssertEqual(dotted.server, "figma")
        XCTAssertEqual(dotted.tool, "get_file")
        let whole = CallFacts(event(1, tool: "Figma__get_file", server: "figma"), delimiter: ".")
        XCTAssertEqual(whole.tool, "Figma__get_file")
        XCTAssertEqual(ToolFacts(delimiter: ".", name: "figma.get_file", server: "figma").shortName, "get_file")
        XCTAssertEqual(ToolCatalog.groups(ofNames: ["Gmail.search", "Drive.list"], delimiter: "."), ["Drive", "Gmail"])
    }

    func testCallFactsKeepAnUnprefixedName() {
        let call = CallFacts(event(1, tool: "search", server: "notion"), delimiter: "__")
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
            tool: "Notion__search", clientType: "ClaudeCode", target: "claude-code", latencyMs: 30_000,
            outcome: .error, reason: "upstream request timed out"
        ))
        XCTAssertEqual(failed.result, "Failed")
        XCTAssertEqual(failed.caller, "Claude Code")
        XCTAssertEqual(failed.reason, "upstream request timed out")
        XCTAssertNotNil(failed.advice)
        XCTAssertNotEqual(failed.advice, Explain.fallback, "a timeout has advice of its own")
        XCTAssertEqual(failed.duration, "30.0 s")

        let old = CallFacts(ActivityEvent(
            sequence: 2, occurredAtMs: 1, client: nil, method: "tools/call", server: "notion",
            latencyMs: 12, outcome: .error
        ))
        XCTAssertNil(old.reason)
        XCTAssertNotNil(old.advice, "a failure always has a next step")
        XCTAssertEqual(old.duration, "12 ms")
    }

    func testACallIsPicturedAsTheClientTheDaemonTookItFor() {
        let call = CallFacts(ActivityEvent(
            sequence: 1, occurredAtMs: 1, client: nil, method: "tools/call", server: "notion",
            clientType: "Unknown", clientLabel: "mcp", clientKey: "codex-cli", target: "codex-cli",
            latencyMs: 1, outcome: .success
        ))
        XCTAssertEqual(call.callerTarget, "codex-cli")
        XCTAssertEqual(call.caller, "Codex CLI")
    }

    func testACancelledCallIsNotAFailure() {
        func call(_ outcome: CallOutcome) -> CallFacts {
            CallFacts(ActivityEvent(
                sequence: 1, occurredAtMs: 1, client: nil, method: "tools/call", server: "notion",
                latencyMs: 12, outcome: outcome
            ))
        }
        let cancelled = call(.cancelled)
        XCTAssertTrue(cancelled.cancelled)
        XCTAssertFalse(cancelled.failed)
        XCTAssertFalse(cancelled.succeeded)
        XCTAssertEqual(cancelled.result, "Canceled")
        XCTAssertEqual(cancelled.advice, "The client stopped this call before it finished.")

        let failed = call(.error)
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

    /// A new sign-in under a name others gave asks which one it is, once.
    func testANewSignInAsksAboutEarlierOnesWithItsName() {
        let now = Date(timeIntervalSince1970: 2_000_000)
        func grant(_ id: String, _ name: String, at created: UInt64, used: UInt64? = 1) -> DownstreamClient {
            let json = #"{"client_id":"\#(id)","client_name":"\#(name)","redirect_uris":[],"source":"dynamic","created_at":\#(created),"expires_at":3000000,"signed_in":true,"client_key":"oauth:\#(id)"}"#
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            var client = try! decoder.decode(DownstreamClient.self, from: Data(json.utf8))
            client.lastUsedAt = used
            return client
        }
        let old = grant("old", "Cursor", at: 1_000_000)
        let new = grant("new", "cursor", at: 1_990_000)
        var unfinished = grant("half", "Cursor", at: 1_500_000, used: nil)
        unfinished.signedIn = false
        let all = [old, new, unfinished, grant("other", "Claude", at: 1_000_000)]

        XCTAssertTrue(unfinished.isUnfinished)
        XCTAssertEqual(
            SignInFacts.earlier(than: new, among: all, ownerNamed: false, answered: [], now: now).map(\.clientId),
            ["old"]
        )
        XCTAssertTrue(SignInFacts.earlier(than: old, among: all, ownerNamed: false, answered: [], now: now).isEmpty)
        XCTAssertTrue(SignInFacts.earlier(than: new, among: all, ownerNamed: true, answered: [], now: now).isEmpty)
        XCTAssertTrue(SignInFacts.earlier(than: new, among: all, ownerNamed: false, answered: ["new"], now: now).isEmpty)
        let later = now.addingTimeInterval(SignInFacts.newFor)
        XCTAssertTrue(SignInFacts.earlier(than: new, among: all, ownerNamed: false, answered: [], now: later).isEmpty)
    }

    func testAnUnfinishedSignInSaysWhenItGoes() {
        let now = Date(timeIntervalSince1970: 1_000_000)
        XCTAssertEqual(SignInFacts.goneIn(expiresAt: 1_000_030, now: now), "gone soon")
        XCTAssertEqual(SignInFacts.goneIn(expiresAt: 1_001_500, now: now), "gone in 25 min")
        XCTAssertEqual(SignInFacts.goneIn(expiresAt: 1_007_300, now: now), "gone in 2 hr")
        XCTAssertEqual(SignInFacts.day(1_000_000, now: now), "today")
    }
}
