import Foundation
import XCTest
@testable import Plug
import PlugIPC

/// `testdata/clients.json` is the daemon's client registry, written by
/// `plug-core/tests/client_registry_golden.rs`.
final class ClientIdentityTests: XCTestCase {
    private struct Entry: Decodable {
        let slug: String
        let name: String
        let aliases: [String]
    }

    private func registry() throws -> [Entry] {
        let root = URL(fileURLWithPath: "\(#filePath)")
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        return try JSONDecoder().decode(
            [Entry].self, from: Data(contentsOf: root.appending(path: "testdata/clients.json"))
        )
    }

    func testEveryRegistryClientIsNamedAsTheDaemonNamesIt() throws {
        let entries = try registry()
        XCTAssertEqual(
            ClientIdentity.registryNames,
            Dictionary(uniqueKeysWithValues: entries.map { ($0.slug, $0.name) })
        )
    }

    /// A slug the app gives a remote product must not be a name the daemon
    /// gives a client of its own: `gemini` is Gemini CLI.
    func testRemoteProductsTakeNoRegistryName() throws {
        let taken = Set(try registry().flatMap { [$0.slug] + $0.aliases })
        XCTAssertTrue(taken.isDisjoint(with: ClientIdentity.remoteNames.keys))
    }

    func testEveryRegistryNameIsGuessedAsItsOwnClient() throws {
        for entry in try registry() {
            XCTAssertEqual(ClientIdentity.guess(from: entry.name), entry.slug, entry.name)
        }
    }

    /// Only what the daemon cannot name is guessed: remote products, and
    /// names a client reports about itself.
    func testNamesTheDaemonCannotPlaceAreGuessed() {
        XCTAssertEqual(ClientIdentity.guess(from: "openai-mcp 1.0.0"), "chatgpt")
        XCTAssertEqual(ClientIdentity.guess(from: "codex-mcp-client"), "codex-cli")
        XCTAssertEqual(ClientIdentity.guess(from: "Grok Bot"), "grok-bot")
        XCTAssertEqual(ClientIdentity.guess(from: "Perplexity"), "perplexity")
        XCTAssertEqual(ClientIdentity.guess(from: "Mistral Le Chat"), "le-chat")
        XCTAssertEqual(ClientIdentity.guess(from: "Muse"), "muse")
        XCTAssertEqual(ClientIdentity.guess(from: "hermes-agent"), "hermes")
        XCTAssertEqual(ClientIdentity.guess(from: "Gemini"), "gemini-cli")
        XCTAssertEqual(ClientIdentity.guess(from: "windsurf-client"), "devin")
        XCTAssertEqual(ClientIdentity.guess(from: "VS Code Copilot"), "vscode")
        XCTAssertEqual(ClientIdentity.guess(from: "some_new_client"), "some-new-client")
        XCTAssertEqual(ClientIdentity.guess(from: "rapid-api-tool"), "rapid-api-tool", "pi is a word, not a letter pair")
        XCTAssertNil(ClientIdentity.displayName(forTarget: "some-new-client"))
    }

    /// The owner's word first, then the daemon's, then what the client says,
    /// then the name on its sign-in.
    func testTheWitnessesRankInOneOrder() {
        XCTAssertEqual(
            ClientIdentity.resolve(ownerName: "Grok Bot", daemonTarget: "cursor", reported: "Cursor", grantName: nil),
            "grok-bot"
        )
        XCTAssertEqual(
            ClientIdentity.resolve(ownerName: "my laptop", daemonTarget: "zed", reported: "Cursor", grantName: nil),
            "zed"
        )
        XCTAssertEqual(
            ClientIdentity.resolve(ownerName: nil, daemonTarget: nil, reported: "Claude Code", grantName: "ChatGPT"),
            "claude-code"
        )
        XCTAssertEqual(
            ClientIdentity.resolve(ownerName: nil, daemonTarget: nil, reported: "Unknown", grantName: "ChatGPT"),
            "chatgpt"
        )
    }

    /// A session the daemon names gets the registry's name for it.
    func testADaemonNamedSessionGetsItsRegistryName() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let session = try decoder.decode(
            LiveSession.self,
            from: Data(#"{"transport":"daemon_proxy","session_id":"z1","client_type":"Unknown","target":"junie","connected_secs":1}"#.utf8)
        )
        XCTAssertEqual(session.knownName, "JetBrains Junie")
    }
}
