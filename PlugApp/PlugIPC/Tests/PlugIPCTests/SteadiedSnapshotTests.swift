import XCTest
@testable import PlugIPC

final class SteadiedSnapshotTests: XCTestCase {
    private func snapshot(uptime: UInt64, connected: UInt64, expires: UInt64?, authenticated: Bool = true) -> OperatorSnapshot {
        var snapshot = OperatorSnapshot.empty
        snapshot.uptimeSecs = uptime
        snapshot.liveSessions = [
            LiveSession(
                transport: "ipc", clientId: nil, sessionId: "one", clientType: "claude-code",
                clientInfo: nil, host: nil, connectedSecs: connected, lastActivitySecs: connected
            ),
        ]
        snapshot.upstreamAuth = [
            AuthServer(
                name: "notes", url: nil, authenticated: authenticated, health: "healthy",
                scopes: nil, tokenExpiresInSecs: expires, warnings: []
            ),
        ]
        return snapshot
    }

    func testTwoReadsAMomentApartAreEqual() {
        let first = snapshot(uptime: 5_002, connected: 301, expires: 90_010)
        let second = snapshot(uptime: 5_004, connected: 303, expires: 90_008)
        XCTAssertNotEqual(first, second)
        XCTAssertEqual(first.steadied(), second.steadied())
    }

    func testARealChangeStillShows() {
        let first = snapshot(uptime: 5_002, connected: 301, expires: 90_010)
        let second = snapshot(uptime: 5_004, connected: 303, expires: 90_008, authenticated: false)
        XCTAssertNotEqual(first.steadied(), second.steadied())
    }

    func testTheFirstMinuteIsOneValueAndItsEndShows() {
        XCTAssertEqual(snapshot(uptime: 7, connected: 0, expires: nil).steadied().uptimeSecs, 0)
        XCTAssertEqual(snapshot(uptime: 60, connected: 0, expires: nil).steadied().uptimeSecs, 0)
        XCTAssertEqual(snapshot(uptime: 61, connected: 0, expires: nil).steadied().uptimeSecs, 61)
        XCTAssertEqual(snapshot(uptime: 185, connected: 0, expires: nil).steadied().uptimeSecs, 180)
    }
}
