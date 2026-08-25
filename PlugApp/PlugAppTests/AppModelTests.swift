import XCTest
@testable import Plug

final class AppModelTests: XCTestCase {
    @MainActor func testEmptyModelIsQuietlyDisconnected() {
        let model = AppModel()
        XCTAssertEqual(model.connectionState, .disconnected)
        XCTAssertTrue(model.visibleServers.isEmpty)
    }
}
