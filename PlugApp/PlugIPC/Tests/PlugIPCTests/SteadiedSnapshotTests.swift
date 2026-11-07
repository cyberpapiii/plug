import XCTest
@testable import PlugIPC

final class SteadiedSnapshotTests: XCTestCase {
    private func snapshot(uptime: UInt64, expires: UInt64?, authenticated: Bool = true) -> OperatorSnapshot {
        var snapshot = OperatorSnapshot.empty
        snapshot.uptimeSecs = uptime
        snapshot.upstreamAuth = [
            AuthServer(
                name: "notes", url: nil, authenticated: authenticated, health: .healthy,
                scopes: nil, tokenExpiresInSecs: expires, warnings: []
            ),
        ]
        return snapshot
    }

    func testTwoReadsAMomentApartAreEqual() {
        let first = snapshot(uptime: 5_002, expires: 90_010)
        let second = snapshot(uptime: 5_004, expires: 90_008)
        XCTAssertNotEqual(first, second)
        XCTAssertEqual(first.steadied(startup: 60), second.steadied(startup: 60))
    }

    func testARealChangeStillShows() {
        let first = snapshot(uptime: 5_002, expires: 90_010)
        let second = snapshot(uptime: 5_004, expires: 90_008, authenticated: false)
        XCTAssertNotEqual(first.steadied(startup: 60), second.steadied(startup: 60))
    }

    func testStartupIsOneValueAndItsEndShows() {
        for startup: UInt64 in [60, 90] {
            XCTAssertEqual(snapshot(uptime: 7, expires: nil).steadied(startup: startup).uptimeSecs, 0)
            XCTAssertEqual(snapshot(uptime: startup, expires: nil).steadied(startup: startup).uptimeSecs, 0)
            XCTAssertEqual(snapshot(uptime: startup + 1, expires: nil).steadied(startup: startup).uptimeSecs, startup + 1)
        }
        XCTAssertEqual(snapshot(uptime: 185, expires: nil).steadied(startup: 60).uptimeSecs, 180)
    }
}
