import XCTest
import PlugIPC
@testable import Plug

/// A second account is named before the daemon is asked, so the sheet and the
/// daemon have to agree on what a name is and on what is sent.
final class AccountDraftTests: XCTestCase {
    /// What the owner types is trimmed and lowercased before the daemon's
    /// rule, which `DraftRulesTests` holds to the daemon, is applied.
    func testWhatIsTypedIsTrimmedAndLowercased() {
        XCTAssertEqual(AccountDraft.label(from: " Personal "), "personal")
        XCTAssertNil(AccountDraft.label(from: "work team"))
    }

    func testAddingAnAccountNamesTheServerAndTheAccount() throws {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let data = try encoder.encode(OperatorMutation.addAccount(server: "slack", account: "work"))
        let sent = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: String])
        XCTAssertEqual(sent, ["type": "AddAccount", "server": "slack", "account": "work"])
    }
}
