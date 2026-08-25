import XCTest
@testable import Plug

final class AppModelTests: XCTestCase {
    @MainActor func testEmptyModelIsQuietlyDisconnected() {
        let model = AppModel()
        XCTAssertEqual(model.connectionState, .disconnected)
        XCTAssertTrue(model.visibleServers.isEmpty)
    }

    @MainActor func testLegacyConnectorDiscoveryOnlyTargetsPlugConnectProcesses() {
        let output = """
          101 /Users/me/.local/bin/plug connect
          102 /Users/me/.cargo/bin/plug --config /tmp/config.toml connect
          103 /Users/me/.local/bin/plug serve --daemon
          104 /usr/bin/python watchdog.py -- /Users/me/.local/bin/plug connect
          105 /bin/zsh -c rg 'plug connect'
        """
        XCTAssertEqual(DaemonServiceManager.connectorPIDs(psOutput: output), [101, 102])
    }
}
