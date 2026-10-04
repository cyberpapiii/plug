import XCTest
import PlugIPC
@testable import Plug

/// A second account is named before the daemon is asked, so the sheet and the
/// daemon have to agree on what a name is and on what is sent.
final class AccountDraftTests: XCTestCase {
    func testAnAccountNameIsLowercaseLettersAndDigits() {
        XCTAssertEqual(AccountDraft.label(from: " Personal "), "personal")
        XCTAssertEqual(AccountDraft.label(from: "team2"), "team2")
        XCTAssertNil(AccountDraft.label(from: ""))
        XCTAssertNil(AccountDraft.label(from: "2nd"))
        XCTAssertNil(AccountDraft.label(from: "work team"))
        XCTAssertNil(AccountDraft.label(from: "work-team"))
        XCTAssertNil(AccountDraft.label(from: String(repeating: "a", count: AccountDraft.longest + 1)))
        XCTAssertEqual(AccountDraft.serverName(server: "slack", label: "work"), "slack-work")
    }

    func testAddingAnAccountNamesTheServerAndTheAccount() throws {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let data = try encoder.encode(IPCRequest.addAccount(authToken: "t", server: "slack", account: "work"))
        let sent = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: String])
        XCTAssertEqual(sent, ["type": "AddAccount", "auth_token": "t", "server": "slack", "account": "work"])
    }
}
