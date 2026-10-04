import XCTest
@testable import Plug

/// The guide ticks a step off when it is done and opens by itself only for a
/// newcomer, so these pin both.
final class FirstRunGuideTests: XCTestCase {
    func testTheGuidePointsAtTheFirstStepNotDone() {
        var guide = FirstRunGuide(serverCount: 0, clientCount: 0, hasActivity: false)
        XCTAssertEqual(guide.next, .server)
        guide = FirstRunGuide(serverCount: 2, clientCount: 0, hasActivity: false)
        XCTAssertTrue(guide.isDone(.server))
        XCTAssertEqual(guide.next, .client)
        // A client that called a tool before any was linked here still counts.
        guide = FirstRunGuide(serverCount: 2, clientCount: 0, hasActivity: true)
        XCTAssertEqual(guide.next, .client)
        guide = FirstRunGuide(serverCount: 2, clientCount: 1, hasActivity: true)
        XCTAssertNil(guide.next)
        XCTAssertTrue(guide.isComplete)
    }

    func testTheGuideOpensByItselfOnlyForANewcomer() {
        XCTAssertTrue(FirstRunGuide.opensByItself(seen: false, loaded: true, serverCount: 0))
        XCTAssertFalse(FirstRunGuide.opensByItself(seen: true, loaded: true, serverCount: 0))
        // Before the daemon answers, an empty list does not mean a new Mac.
        XCTAssertFalse(FirstRunGuide.opensByItself(seen: false, loaded: false, serverCount: 0))
        XCTAssertFalse(FirstRunGuide.opensByItself(seen: false, loaded: true, serverCount: 3))
    }

    func testTheAgentPromptShipsInTheApp() throws {
        let prompt = try XCTUnwrap(FirstRunGuide.agentPrompt)
        XCTAssertTrue(prompt.contains("plug import --dry-run"))
        XCTAssertTrue(prompt.contains("plug link --yes"))
    }
}
