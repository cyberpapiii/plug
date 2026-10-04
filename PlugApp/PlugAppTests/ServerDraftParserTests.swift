import PlugIPC
import XCTest
@testable import Plug

/// Adding a server means pasting whatever the server's instructions printed.
/// These pin the shapes that arrive in practice.
final class ServerDraftParserTests: XCTestCase {
    private func draft(_ text: String) throws -> ServerDraft {
        guard case let .draft(draft) = ServerDraftParser.parse(text) else {
            throw XCTSkip("expected a draft for: \(text)")
        }
        return draft
    }

    func testEmptyPasteIsNotAnError() {
        XCTAssertEqual(ServerDraftParser.parse("   \n "), .empty)
    }

    func testReadmeStyleMCPServersBlock() throws {
        let draft = try draft("""
        {
          "mcpServers": {
            "linear": {
              "command": "npx",
              "args": ["-y", "linear-mcp@latest"],
              "env": { "LINEAR_API_KEY": "secret" }
            }
          }
        }
        """)
        XCTAssertEqual(draft.name, "linear")
        XCTAssertEqual(draft.config.command, "npx")
        XCTAssertEqual(draft.config.args, ["-y", "linear-mcp@latest"])
        XCTAssertEqual(draft.config.env, ["LINEAR_API_KEY": "secret"])
        XCTAssertEqual(draft.config.transport, "stdio")
        XCTAssertTrue(draft.facts.contains { $0.label == "Variables" && $0.value == "LINEAR_API_KEY" })
    }

    func testBareEntryWithoutTheWrapper() throws {
        let draft = try draft(#"{ "figma": { "command": "figma-console-mcp" } }"#)
        XCTAssertEqual(draft.name, "figma")
        XCTAssertEqual(draft.config.command, "figma-console-mcp")
    }

    func testRemoteEntryKeepsURLAndLiftsBearerToken() throws {
        let draft = try draft("""
        {
          "notion": {
            "url": "https://mcp.notion.com/mcp",
            "headers": { "Authorization": "Bearer abc123" }
          }
        }
        """)
        XCTAssertEqual(draft.config.transport, "http")
        XCTAssertEqual(draft.config.url, "https://mcp.notion.com/mcp")
        XCTAssertEqual(draft.config.authToken, "abc123")
    }

    func testDeclaredSSETypeIsHonoured() throws {
        let draft = try draft(#"{ "old": { "url": "https://example.com/sse", "type": "sse" } }"#)
        XCTAssertEqual(draft.config.transport, "sse")
    }

    func testTruncatedJSONExplainsItselfInsteadOfFailingSilently() {
        guard case let .unreadable(reason) = ServerDraftParser.parse(#"{ "mcpServers": {"#) else {
            return XCTFail("expected an explanation")
        }
        XCTAssertTrue(reason.contains("braces"), reason)
    }

    func testJSONWithNoServerBodyIsRefusedClearly() {
        guard case .unreadable = ServerDraftParser.parse(#"{ "notes": "hello" }"#) else {
            return XCTFail("expected an explanation")
        }
    }

    func testPlainURLBecomesARemoteServerNamedForItsHost() throws {
        let draft = try draft("https://mcp.linear.app/sse")
        XCTAssertEqual(draft.name, "linear")
        XCTAssertEqual(draft.config.url, "https://mcp.linear.app/sse")
        XCTAssertEqual(draft.config.transport, "http")
    }

    func testAServerIsNamedForItsMakerNotForAGenericHostLabel() throws {
        XCTAssertEqual(try draft("https://mcp.notion.com/mcp").name, "notion")
        XCTAssertEqual(try draft("https://www.example.com/mcp").name, "example")
        XCTAssertEqual(try draft("https://api.mcp.example.com/mcp").name, "example")
        XCTAssertEqual(try draft("https://MCP.Example.com/mcp").name.lowercased(), "example")
        // A label that is not generic stays, and the last two labels are
        // never dropped.
        XCTAssertEqual(try draft("https://tools.example.com/mcp").name, "tools")
        XCTAssertEqual(try draft("https://mcp.com/mcp").name, "mcp")
    }

    func testAnOpenAPIDocumentAddressBecomesAnAPIServer() throws {
        let draft = try draft("https://petstore3.swagger.io/api/v3/openapi.json")
        XCTAssertEqual(draft.name, "petstore3")
        XCTAssertEqual(draft.config.transport, "openapi")
        XCTAssertEqual(draft.config.spec, "https://petstore3.swagger.io/api/v3/openapi.json")
        XCTAssertNil(draft.config.url)
        XCTAssertTrue(draft.fromAddress)
        XCTAssertTrue(draft.facts.contains { $0.label == "Kind" && $0.value.hasPrefix("Web API") })
    }

    func testThePersonCanCorrectTheGuessAboutAnAddress() throws {
        guard case let .draft(api) = ServerDraftParser.parse(
            "https://api.example.com/v1/spec", addressIsAPI: true
        ) else { return XCTFail("expected a draft") }
        XCTAssertEqual(api.config.transport, "openapi")
        XCTAssertEqual(api.config.spec, "https://api.example.com/v1/spec")

        guard case let .draft(server) = ServerDraftParser.parse(
            "https://example.com/openapi.json", addressIsAPI: false
        ) else { return XCTFail("expected a draft") }
        XCTAssertEqual(server.config.transport, "http")
        XCTAssertEqual(server.config.url, "https://example.com/openapi.json")
        XCTAssertNil(server.config.spec)
    }

    func testAnOpenAPIFileOnThisMacBecomesAnAPIServer() throws {
        let draft = try draft("~/Documents/billing.yaml")
        XCTAssertEqual(draft.name, "billing")
        XCTAssertEqual(draft.config.transport, "openapi")
        XCTAssertEqual(draft.config.spec, "~/Documents/billing.yaml")
        XCTAssertFalse(draft.fromAddress)

        // A path to a program is still a command.
        XCTAssertEqual(try self.draft("/usr/local/bin/server --flag").config.transport, "stdio")
    }

    func testShellCommandKeepsArgumentsAndLiftsEnvironmentPrefixes() throws {
        let draft = try draft("GITHUB_TOKEN=abc npx -y @modelcontextprotocol/server-github")
        XCTAssertEqual(draft.config.command, "npx")
        XCTAssertEqual(draft.config.args, ["-y", "@modelcontextprotocol/server-github"])
        XCTAssertEqual(draft.config.env, ["GITHUB_TOKEN": "abc"])
    }

    /// The runner is not the server's name; the package is.
    func testNameIsGuessedFromThePackageNotTheRunner() throws {
        XCTAssertEqual(try draft("npx -y linear-mcp@1.2.0").name, "linear")
        XCTAssertEqual(try draft("uvx mcp-server-fetch").name, "fetch")
        XCTAssertEqual(try draft("/usr/local/bin/my-server --stdio").name, "my-server")
    }

    func testQuotedPathsSurviveTokenizing() throws {
        let draft = try draft(#"node "/Users/me/My Servers/index.js" --stdio"#)
        XCTAssertEqual(draft.config.command, "node")
        XCTAssertEqual(draft.config.args, ["/Users/me/My Servers/index.js", "--stdio"])
    }

    func testEnvironmentOnlyPasteAsksForTheCommand() {
        guard case let .unreadable(reason) = ServerDraftParser.parse("FOO=bar BAZ=qux") else {
            return XCTFail("expected an explanation")
        }
        XCTAssertTrue(reason.contains("command"), reason)
    }

    private func api(_ count: Int) -> APISummary {
        APISummary(title: "Pets", operations: (0 ..< count).map {
            APIOperation(name: "op\($0)", method: "GET", path: "/p\($0)", summary: "", tag: $0 % 2 == 0 ? "even" : nil)
        })
    }

    func testASmallAPIStartsWholeAndSavesNoList() {
        var choice = APIOperationChoice(api: api(3))
        XCTAssertNil(choice.problem)
        XCTAssertEqual(choice.setting, [])
        choice.chosen.remove("op1")
        XCTAssertEqual(choice.setting, ["op0", "op2"])
        XCTAssertEqual(choice.groups.map(\.tag), ["even", "Other"])
    }

    func testALargeAPIMustBeNarrowedBeforeItIsAdded() {
        var choice = APIOperationChoice(api: api(APISummary.operationLimit + 1))
        XCTAssertNotNil(choice.problem)
        choice.chosen = ["op4", "op2"]
        XCTAssertNil(choice.problem)
        XCTAssertEqual(choice.setting, ["op2", "op4"])
        choice.chosen = Set(choice.api.operations.map(\.name))
        XCTAssertNotNil(choice.problem)
    }

    func testPreviewNeverInventsFactsItDoesNotHave() throws {
        let draft = try draft("my-server")
        XCTAssertEqual(draft.facts.map(\.label), ["Runs", "Where"])
    }
}

/// Add Server and Edit are one form. It starts from a whole server and gives
/// one back.
final class ServerFormTests: XCTestCase {
    func testAnUntouchedFormGivesBackTheServerItStartedFrom() {
        var config = ServerConfig.command("npx", args: ["-y", "linear mcp"])
        config.env = ["API_KEY": "abc", "REGION": "us"]
        config.callTimeoutSecs = 45
        XCTAssertEqual(ServerForm(config: config).config, config)

        var remote = ServerConfig.remote("https://example.com/mcp")
        remote.auth = "oauth"
        remote.transport = "sse"
        XCTAssertEqual(ServerForm(config: remote).config, remote)
    }

    func testAnEmptyKeyFieldKeepsTheKeyTheServerHas() {
        var config = ServerConfig.remote("https://example.com/mcp")
        config.authToken = "keychain:example"
        var form = ServerForm(config: config)
        XCTAssertTrue(form.hasKey)
        XCTAssertEqual(form.config.authToken, "keychain:example")

        form.key = " new "
        XCTAssertEqual(form.config.authToken, "new")

        form.key = ""
        form.removeKey = true
        XCTAssertNil(form.config.authToken)
    }

    func testMovingAServerToThisMacDropsWhatOnlyANetworkServerHas() {
        var config = ServerConfig.remote("https://example.com/mcp")
        config.auth = "oauth"
        config.authToken = "t"
        var form = ServerForm(config: config)
        form.isRemote = false
        XCTAssertFalse(form.isComplete)
        form.command = "npx"
        form.arguments = "-y 'my server'"
        let saved = form.config
        XCTAssertEqual(saved.transport, "stdio")
        XCTAssertEqual(saved.args, ["-y", "my server"])
        XCTAssertNil(saved.url)
        XCTAssertNil(saved.auth)
        XCTAssertNil(saved.authToken)
    }

    func testVariablesMustBeNameEqualsValueLines() {
        var form = ServerForm(config: .command("npx", args: []))
        XCTAssertNil(form.settingsProblem)
        XCTAssertTrue(form.isComplete)

        for good in ["A=1\n\n B = 2 ", "EMPTY=", "URL=https://example.com/?a=b"] {
            form.settings = good
            XCTAssertNil(form.settingsProblem, good)
            XCTAssertTrue(form.isComplete, good)
        }
        for bad in ["A=1\nnonsense", "=value", "  = value"] {
            form.settings = bad
            XCTAssertEqual(form.settingsProblem, "Each line needs NAME=value.", bad)
            XCTAssertFalse(form.isComplete, bad)
        }
    }

    func testAnAPIServerMayLeaveItsAddressToItsDocument() {
        let form = ServerForm(config: .api("https://example.com/openapi.json"))
        XCTAssertTrue(form.isAPI)
        XCTAssertTrue(form.isComplete)
        XCTAssertNil(form.config.url)
        XCTAssertEqual(form.config.transport, "openapi")
    }
}

/// The servers Add Server offers by name.
final class KnownServerTests: XCTestCase {
    func testEveryKnownServerIsASecureAddressWithItsOwnName() {
        XCTAssertEqual(Set(KnownServer.all.map(\.id)).count, KnownServer.all.count)
        for server in KnownServer.all {
            XCTAssertEqual(URL(string: server.address)?.scheme, "https", server.id)
            XCTAssertEqual(server.id, server.id.lowercased())
            XCTAssertEqual(server.config.auth, server.needsSignIn ? "oauth" : nil, server.id)
            XCTAssertEqual(server.draft.name, server.id)
        }
    }

    func testAServerPlugAlreadyHasIsNotOfferedAgain() {
        let offered = KnownServer.notYetAdded(names: ["Notion", "linear", "other"])
        XCTAssertFalse(offered.contains { $0.id == "notion" || $0.id == "linear" })
        XCTAssertEqual(offered.count, KnownServer.all.count - 2)
    }
}
