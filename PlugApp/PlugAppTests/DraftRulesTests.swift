import XCTest
import PlugIPC
@testable import Plug

/// `testdata/draft_rules.json` holds the daemon's rules for what the app
/// checks before sending a draft; `plug-core/tests/draft_rules_golden.rs`
/// checks the daemon against the same file.
final class DraftRulesTests: XCTestCase {
    private struct Rules: Decodable {
        struct Account: Decodable {
            let server: String
            let account: String
            let name: String?
        }
        struct CustomClient: Decodable {
            let named: String
            let target: String?
        }
        let apiOperationLimit: Int
        let accounts: [Account]
        let customClientPrefix: String
        let customClients: [CustomClient]
    }

    private func rules() throws -> Rules {
        let root = URL(fileURLWithPath: "\(#filePath)")
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Rules.self, from: Data(contentsOf: root.appending(path: "testdata/draft_rules.json")))
    }

    func testAnAPIServerTakesTheDaemonsNumberOfOperations() throws {
        XCTAssertEqual(try rules().apiOperationLimit, APISummary.operationLimit)
    }

    func testAnAccountIsNamedTheWayTheDaemonNamesIt() throws {
        for account in try rules().accounts {
            let label = AccountDraft.label(from: account.account)
            XCTAssertEqual(
                label.map { AccountDraft.serverName(server: account.server, label: $0) },
                account.name,
                account.account
            )
        }
    }

    func testANamedClientGetsAKeyTheDaemonAccepts() throws {
        let rules = try rules()
        XCTAssertEqual(rules.customClientPrefix, AddClient.customPrefix)
        for client in rules.customClients {
            XCTAssertEqual(AddClient.customTarget(named: client.named), client.target, client.named)
        }
    }
}
