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
          "scanned": [{ "source": "Cursor", "source_target": "cursor", "source_name": "Cursor", "servers": [], "error": null }],
          "new_servers": [
            {
              "name": "linear",
              "source": "Cursor", "source_target": "cursor", "source_name": "Cursor",
              "config": {
                "command": "npx", "args": ["-y", "linear-mcp"],
                "env": { "LINEAR_API_KEY": "lin_1" }, "url": null
              }
            },
            {
              "name": "notion",
              "source": "ClaudeDesktop", "source_target": "claude-desktop", "source_name": "Claude Desktop",
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
              "source": "Cursor", "source_target": "cursor", "source_name": "Cursor",
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
            { "name": "broken", "source": "Zed", "source_target": "zed", "source_name": "Zed", "config": { "command": null, "args": [], "env": {}, "url": null } },
            { "name": "", "source": "Zed", "source_target": "zed", "source_name": "Zed", "config": { "command": "npx", "args": [], "env": {} } }
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
            { "source": "VSCodeCopilot", "source_target": "vscode", "source_name": "VS Code Copilot", "servers": [], "error": "invalid JSON" },
            { "source": "Cursor", "source_target": "cursor", "source_name": "Cursor", "servers": [], "error": null },
            { "source": "ClaudeCode", "source_target": "claude-code", "source_name": "Claude Code", "servers": [], "error": "permission denied" }
          ],
          "new_servers": []
        }
        """)
        XCTAssertEqual(result.unreadable, ["Claude Code", "VS Code Copilot"])
        XCTAssertTrue(result.isEmpty)
    }

    func testRefusesAnswersItCannotRead() {
        XCTAssertThrowsError(try scan("not json"))
    }
}
