import XCTest
@testable import Plug

final class AppModelTests: XCTestCase {
    @MainActor func testEmptyModelIsQuietlyDisconnected() {
        let model = AppModel()
        XCTAssertEqual(model.connectionState, .disconnected)
        XCTAssertTrue(model.visibleServers.isEmpty)
    }

    @MainActor func testDaemonOwnershipRequiresThisAppBundle() {
        XCTAssertTrue(DaemonServiceManager.isAppManaged(
            launchctlOutput: "program = /Applications/Plug.app/Contents/Resources/plug",
            bundlePath: "/Applications/Plug.app"
        ))
        XCTAssertFalse(DaemonServiceManager.isAppManaged(
            launchctlOutput: "program = /tmp/old/plug",
            bundlePath: "/Applications/Plug.app"
        ))
    }
}
