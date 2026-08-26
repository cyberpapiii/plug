import XCTest
import PlugIPC
@testable import Plug

/// Reading another app's settings is the one place Plug interprets a file it
/// does not own, so what it makes of that report is worth pinning down.
final class ImportScanTests: XCTestCase {
    private func scan(_ json: String) throws -> ImportScan {
        try ImportScan(json: Data(json.utf8))
    }

    func testReadsLocalAndRemoteServers() throws {
        let result = try scan("""
        {
          "scanned": [{ "source": "Cursor", "servers": [], "error": null }],
          "new_servers": [
            {
              "name": "linear",
              "source": "Cursor",
              "config": {
                "command": "npx", "args": ["-y", "linear-mcp"],
                "env": { "LINEAR_API_KEY": "lin_1" }, "url": null
              }
            },
            {
              "name": "notion",
              "source": "ClaudeDesktop",
              "config": { "command": null, "args": [], "env": {}, "url": "https://mcp.notion.com/mcp" }
            }
          ]
        }
        """)

        XCTAssertEqual(result.servers.count, 2)
        let linear = result.servers[0]
        XCTAssertEqual(linear.name, "linear")
        XCTAssertEqual(linear.source, "cursor")
        XCTAssertEqual(linear.sourceName, "Cursor")
        XCTAssertEqual(linear.config.command, "npx")
        XCTAssertEqual(linear.config.args, ["-y", "linear-mcp"])
        XCTAssertEqual(linear.config.env["LINEAR_API_KEY"], "lin_1")
        XCTAssertEqual(linear.config.transport, "stdio")
        XCTAssertEqual(linear.detail, "npx -y linear-mcp")

        let notion = result.servers[1]
        XCTAssertEqual(notion.source, "claude-desktop")
        XCTAssertEqual(notion.sourceName, "Claude Desktop")
        XCTAssertEqual(notion.config.transport, "http")
        XCTAssertEqual(notion.config.url, "https://mcp.notion.com/mcp")
        XCTAssertEqual(notion.detail, "https://mcp.notion.com/mcp")
    }

    /// Every linked app has a Plug entry of its own. Importing it would point
    /// Plug at itself.
    func testSkipsPlugsOwnEntry() throws {
        let result = try scan("""
        {
          "scanned": [],
          "new_servers": [
            {
              "name": "Plug",
              "source": "Cursor",
              "config": {
                "command": "/Users/someone/.local/bin/plug",
                "args": ["connect"], "env": {}, "url": null
              }
            }
          ]
        }
        """)
        XCTAssertTrue(result.isEmpty)
    }

    func testSkipsEntriesWithNothingToRun() throws {
        let result = try scan("""
        { "scanned": [], "new_servers": [
            { "name": "broken", "source": "Zed", "config": { "command": null, "args": [], "env": {}, "url": null } },
            { "name": "", "source": "Zed", "config": { "command": "npx", "args": [], "env": {} } }
        ] }
        """)
        XCTAssertTrue(result.isEmpty)
    }

    /// An app whose settings could not be read is named, so an empty result is
    /// never mistaken for "you have nothing set up".
    func testNamesAppsItCouldNotRead() throws {
        let result = try scan("""
        {
          "scanned": [
            { "source": "VSCodeCopilot", "servers": [], "error": "invalid JSON" },
            { "source": "Cursor", "servers": [], "error": null },
            { "source": "ClaudeCode", "servers": [], "error": "permission denied" }
          ],
          "new_servers": []
        }
        """)
        XCTAssertEqual(result.unreadable, ["Claude Code", "VS Code"])
        XCTAssertTrue(result.isEmpty)
    }

    func testRefusesAnswersItCannotRead() {
        XCTAssertThrowsError(try scan("not json"))
    }

    func testSourceNamesMapToAppTargets() {
        XCTAssertEqual(ImportScan.target(forSource: "VSCodeCopilot"), "vscode")
        XCTAssertEqual(ImportScan.target(forSource: "OpenCode"), "opencode")
        XCTAssertEqual(ImportScan.target(forSource: "CodexCli"), "codex-cli")
        XCTAssertEqual(ImportScan.target(forSource: "Windsurf"), "windsurf")
        XCTAssertEqual(ImportScan.appName(fromSource: "GeminiCli"), "Gemini CLI")
        XCTAssertEqual(ImportScan.appName(fromSource: "Zed"), "Zed")
    }
}
