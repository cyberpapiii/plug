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

    func testARowShowsTheTitleTheRuntimeSends() throws {
        let result = try checkup(#"""
        {"checks":[
          {"name":"config_permissions","title":"Settings file is private","status":"pass","message":""},
          {"name":"brand_new_check","status":"pass","message":""}
        ]}
        """#)
        XCTAssertEqual(result.checks[0].title, "Settings file is private")
        XCTAssertEqual(result.checks[1].title, "brand new check")
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
        XCTAssertEqual(AppIcons.symbol(target: "goose", name: "Goose"), "bird")
    }

    func testEditorsGetAnEditorGlyph() {
        XCTAssertEqual(
            AppIcons.symbol(target: "vscode", name: "VS Code"),
            "chevron.left.forwardslash.chevron.right"
        )
    }

    func testRemoteClientsAndTextAgentsAreNamedAndPictured() {
        XCTAssertEqual(AppIcons.target(forClientType: "gemini-cli-mcp-client"), "gemini-cli")
        XCTAssertEqual(AppIcons.target(forClientType: "Gemini"), "gemini")
        XCTAssertEqual(AppIcons.target(forClientType: "Perplexity"), "perplexity")
        XCTAssertEqual(AppIcons.target(forClientType: "Mistral Le Chat"), "le-chat")
        XCTAssertEqual(AppIcons.target(forClientType: "hermes-agent"), "hermes")
        XCTAssertEqual(AppIcons.displayName(forTarget: "hermes"), "Hermes Agent")
        XCTAssertEqual(AppIcons.displayName(forTarget: "poke"), "Poke")
        XCTAssertEqual(AppIcons.symbol(target: "poke"), "message")
        XCTAssertEqual(AppIcons.symbol(target: "le-chat"), "globe")
    }

    func testAnUnknownAppStillGetsSomethingAppShaped() {
        XCTAssertEqual(AppIcons.symbol(target: "brand-new-thing", name: "Brand New Thing"), "app")
    }

    func testAServerFindsItsAppWhateverThePunctuation() {
        XCTAssertEqual(AppIcons.lookupKey("agent-admin"), AppIcons.lookupKey("AgentAdmin"))
        XCTAssertEqual(AppIcons.lookupKey("Google Drive"), "googledrive")
        XCTAssertEqual(AppIcons.lookupKey("1password"), "1password")
    }

    func testAnOpenAIConnectorIsChatGPT() {
        XCTAssertEqual(AppIcons.target(forClientType: "openai-mcp 1.0.0"), "chatgpt")
        XCTAssertEqual(AppIcons.target(forClientType: "codex-mcp-client"), "codex-cli")
    }

    func testGoogleServersShareOneIconUnlessTheirAppIsHere() {
        XCTAssertEqual(AppIcons.appName(forServer: "workspace"), "googledrive")
        XCTAssertEqual(AppIcons.appName(forServer: "Gmail"), "googledrive")
        XCTAssertEqual(AppIcons.appName(forServer: "GoogleCalendar"), "googledrive")
        XCTAssertEqual(AppIcons.appName(forServer: "GoogleDocs") { $0 == "googledocs" }, "googledocs")
        XCTAssertEqual(AppIcons.appName(forServer: "imessage"), "messages")
        XCTAssertEqual(AppIcons.appName(forServer: "slack"), "slack")
    }

    func testATileKeepsItsLetterAndItsColor() {
        XCTAssertEqual(MonogramTile.letter(for: "workspace"), "W")
        XCTAssertEqual(MonogramTile.letter(for: "-exa"), "E")
        XCTAssertEqual(MonogramTile.letter(for: ""), "?")
        XCTAssertEqual(MonogramTile.tintIndex(for: "Workspace"), MonogramTile.tintIndex(for: "workspace"))
    }

    func testLiveSessionsAreMatchedToTheAppTheyBelongTo() {
        XCTAssertEqual(AppIcons.target(forClientType: "claude_code"), "claude-code")
        XCTAssertEqual(AppIcons.target(forClientType: "Claude Desktop"), "claude-desktop")
        XCTAssertEqual(AppIcons.target(forClientType: "Codex CLI"), "codex-cli")
        XCTAssertEqual(AppIcons.target(forClientType: "codex-mcp-client"), "codex-cli")
        XCTAssertEqual(AppIcons.target(forClientType: "Devin"), "devin")
        XCTAssertEqual(AppIcons.target(forClientType: "Cascade (Devin Desktop)"), "devin")
        XCTAssertEqual(AppIcons.target(forClientType: "windsurf-client"), "devin")
        XCTAssertEqual(AppIcons.displayName(forTarget: "devin"), "Devin")
        XCTAssertEqual(AppIcons.target(forClientType: "Grok Build"), "grok-build")
        XCTAssertEqual(AppIcons.target(forClientType: "GitHub Copilot CLI"), "copilot-cli")
        XCTAssertEqual(AppIcons.target(forClientType: "VS Code Copilot"), "vscode")
        XCTAssertEqual(AppIcons.displayName(forTarget: "copilot-cli"), "GitHub Copilot CLI")
        XCTAssertEqual(AppIcons.target(forClientType: "Grok Bot"), "grok-bot")
        XCTAssertEqual(AppIcons.displayName(forTarget: "grok-bot"), "Grok Bot")
        XCTAssertEqual(AppIcons.symbol(target: "grok-build", name: "Grok Build"), "terminal")
        XCTAssertEqual(AppIcons.target(forClientType: "cursor"), "cursor")
        XCTAssertEqual(AppIcons.target(forClientType: "Some Agent"), "some-agent")
        XCTAssertNil(AppIcons.displayName(forTarget: "some-agent"))
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

/// A server says where it runs in words.
final class ServerPlaceTests: XCTestCase {
    private func server(transport: String) -> ServerFacts {
        ServerFacts(name: "s", enabled: true, transport: transport, health: .working)
    }

    func testLocalAndRemoteReadDifferently() {
        XCTAssertEqual(server(transport: "stdio").transportLabel, "On this Mac")
        XCTAssertEqual(server(transport: "streamable_http").transportLabel, "Over the network")
        XCTAssertEqual(server(transport: "sse").transportLabel, "Over the network")
    }
}

/// Where an icon comes from when no app on this Mac supplies it.
final class FoundIconTests: XCTestCase {
    private func picture(side: Int = 64) throws -> Data {
        let rep = try XCTUnwrap(NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: side, pixelsHigh: side, bitsPerSample: 8,
            samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
            colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
        ))
        return try XCTUnwrap(rep.representation(using: .png, properties: [:]))
    }

    func testAServerStartedFromInsideAnAppIsThatApp() {
        XCTAssertEqual(
            AppIcons.appBundle(containing: "/Applications/Foo Bar.app/Contents/MacOS/foo-mcp"),
            "/Applications/Foo Bar.app"
        )
        XCTAssertNil(AppIcons.appBundle(containing: "/usr/local/bin/foo-mcp"))
        XCTAssertNil(AppIcons.appBundle(containing: "npx"))
    }

    func testOnlyAServersOwnHTTPSSitesAreAsked() {
        XCTAssertEqual(
            SiteIcon.hosts(server: "https://mcp.example.com/mcp?key=1", website: nil),
            ["mcp.example.com", "example.com"]
        )
        XCTAssertEqual(
            SiteIcon.hosts(server: "https://mcp.example.com/mcp", website: "https://developers.example.com/docs"),
            ["mcp.example.com", "example.com", "developers.example.com"]
        )
        XCTAssertEqual(SiteIcon.hosts(server: "http://localhost:8000/mcp", website: nil), [])
        XCTAssertEqual(SiteIcon.hosts(server: "http://127.0.0.1:8080", website: "http://example.com"), [])
        XCTAssertEqual(SiteIcon.hosts(server: "https://10.0.0.12/mcp", website: nil), ["10.0.0.12"])
        XCTAssertEqual(SiteIcon.hosts(server: "https://example.com/mcp", website: nil), ["example.com"])
        XCTAssertEqual(SiteIcon.hosts(server: nil, website: nil), [])
    }

    func testAMakersSiteIsFoundFromAName() {
        XCTAssertEqual(SiteIcon.brandHost(forName: "oura"), "ouraring.com")
        XCTAssertEqual(SiteIcon.brandHost(forName: "Oura-MCP"), "ouraring.com")
        XCTAssertEqual(SiteIcon.brandHost(forName: "python3"), "python.org")
        XCTAssertEqual(SiteIcon.brandHost(forName: "Qwen Code"), "qwen.ai")
        XCTAssertNil(SiteIcon.brandHost(forName: "boxer"), "a word is matched whole")
        XCTAssertNil(SiteIcon.brandHost(forName: "cli"))
    }

    func testAPackageIsReadFromTheCommandThatRunsIt() {
        XCTAssertEqual(
            SiteIcon.package(command: "npx", args: ["-y", "@scope/thing-mcp@1.2.3", "--flag"]),
            SiteIcon.Package(registry: .npm, name: "@scope/thing-mcp")
        )
        XCTAssertEqual(
            SiteIcon.package(command: "/opt/homebrew/bin/npx", args: ["thing-mcp@latest"]),
            SiteIcon.Package(registry: .npm, name: "thing-mcp")
        )
        XCTAssertEqual(
            SiteIcon.package(command: "uvx", args: ["thing-mcp==2.0"]),
            SiteIcon.Package(registry: .pypi, name: "thing-mcp")
        )
        XCTAssertNil(SiteIcon.package(command: "node", args: ["/Users/me/thing/index.js"]))
        XCTAssertNil(SiteIcon.package(command: "npx", args: ["-y"]))
        XCTAssertNil(SiteIcon.package(command: "npx", args: ["../thing?x=1"]), "only a plain name is asked about")
        XCTAssertNil(SiteIcon.package(command: nil, args: []))
    }

    func testAPackagesLinksGiveItsSiteAndItsOwner() {
        let places = SiteIcon.places(forPackageLinks: [
            "https://thing.example.com/docs",
            "git+https://github.com/someone/thing-mcp.git",
            "https://www.npmjs.com/package/thing",
            "http://insecure.example.com",
        ])
        XCTAssertEqual(places.hosts, ["thing.example.com"])
        XCTAssertEqual(places.picture?.absoluteString, "https://github.com/someone.png")
        XCTAssertNil(SiteIcon.places(forPackageLinks: []).picture)
    }

    func testAPagesIconsComeBestFirst() throws {
        let html = """
        <html><head>
        <link rel="stylesheet" href="/site.css">
        <link rel="icon" sizes="32x32" href="/small.png">
        <LINK REL='icon' SIZES='192x192' HREF='https://cdn.example.com/big.png'>
        <link rel=apple-touch-icon href=touch.png>
        <link rel="mask-icon" href="/mask.svg">
        <link rel="icon" href="http://example.com/plain.png">
        </head></html>
        """
        let base = try XCTUnwrap(URL(string: "https://example.com/docs/"))
        XCTAssertEqual(SiteIcon.candidates(inHTML: html, base: base).map(\.absoluteString), [
            "https://example.com/docs/touch.png",
            "https://cdn.example.com/big.png",
            "https://example.com/small.png",
            "https://example.com/apple-touch-icon.png",
            "https://example.com/favicon.ico",
        ])
        XCTAssertEqual(SiteIcon.candidates(inHTML: "", base: base).map(\.path), [
            "/apple-touch-icon.png", "/favicon.ico",
        ])
    }

    func testTheLargestIconAServerOffersIsUsed() throws {
        let icons = [
            ServerIcon(src: "data:image/png;base64,AAAA", sizes: ["16x16"]),
            ServerIcon(src: "data:image/png;base64,QUJD", sizes: ["64x64"]),
            ServerIcon(src: "https://example.com/icon.png", sizes: ["32x32"]),
        ]
        let best = try XCTUnwrap(SiteIcon.best(of: icons))
        XCTAssertEqual(best.src, "data:image/png;base64,QUJD")
        XCTAssertEqual(SiteIcon.data(fromDataURI: best.src), Data("ABC".utf8))
        XCTAssertNil(SiteIcon.data(fromDataURI: "https://example.com/icon.png"))
        XCTAssertNil(SiteIcon.data(fromDataURI: "data:image/png,raw"))
        XCTAssertNil(SiteIcon.best(of: []))
    }

    func testOnlyAPictureBecomesAnIcon() throws {
        XCTAssertNotNil(IconTile.png(from: try picture()))
        XCTAssertNil(IconTile.png(from: Data("<html>not found</html>".utf8)))
        XCTAssertNil(IconTile.png(from: try picture(side: 8)), "too small to show")
    }

    @MainActor
    func testAChosenIconIsKeptAndCanBeTakenBack() throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "plug-icons-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let key = IconStore.key(server: "No Such Server")
        XCTAssertEqual(key, "server-nosuchserver")

        let store = IconStore(chosenDirectory: directory, cacheDirectory: directory.appending(path: "cache"))
        XCTAssertNil(store.image(forServer: "No Such Server"))
        XCTAssertFalse(store.setChosenIcon(Data("nope".utf8), for: key))
        XCTAssertTrue(store.setChosenIcon(try picture(), for: key))
        XCTAssertNotNil(store.image(forServer: "no-such-server"), "the name matches whatever its punctuation")

        let reopened = IconStore(chosenDirectory: directory, cacheDirectory: directory.appending(path: "cache"))
        XCTAssertNotNil(reopened.image(forServer: "No Such Server"))
        reopened.removeChosenIcon(for: key)
        XCTAssertNil(reopened.image(forServer: "No Such Server"))
        XCTAssertNil(IconStore(chosenDirectory: directory, cacheDirectory: directory).image(forServer: "No Such Server"))
    }

    func testTheStatusAnswerCarriesWhatAServerSaysAboutItself() throws {
        let json = """
        {"type":"Status","servers":[
          {"server_id":"a","health":"Healthy","tool_count":3,"auth_status":"none",
           "upstream":{"name":"a","version":"1","website_url":"https://example.com",
                       "icons":[{"src":"data:image/png;base64,QUJD","sizes":["64x64"]}]}},
          {"server_id":"b","health":"Failed","tool_count":0}
        ],"clients":0,"uptime_secs":5}
        """
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        guard case let .status(servers) = try decoder.decode(IPCResponse.self, from: Data(json.utf8)) else {
            return XCTFail("expected a status answer")
        }
        XCTAssertEqual(servers.map(\.serverId), ["a", "b"])
        XCTAssertEqual(servers[0].upstream?.websiteUrl, "https://example.com")
        XCTAssertEqual(servers[0].upstream?.icons?.first?.sizes, ["64x64"])
        XCTAssertNil(servers[1].upstream)
    }
}
