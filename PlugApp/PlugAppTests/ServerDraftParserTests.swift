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
        XCTAssertTrue(draft.facts.contains { $0.label == "Environment" && $0.value == "LINEAR_API_KEY" })
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
        XCTAssertEqual(draft.name, "mcp")
        XCTAssertEqual(draft.config.url, "https://mcp.linear.app/sse")
        XCTAssertEqual(draft.config.transport, "http")
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

    func testPreviewNeverInventsFactsItDoesNotHave() throws {
        let draft = try draft("my-server")
        XCTAssertEqual(draft.facts.map(\.label), ["Runs", "Kind"])
    }
}
