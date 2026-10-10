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

    func testEachFirstIsMarkedOnceOnANewSetup() {
        var firsts = FirstMoments(stored: nil)
        XCTAssertNil(firsts.observe(FirstRunGuide(serverCount: 0, clientCount: 0, hasActivity: false)))
        XCTAssertEqual(firsts.observe(FirstRunGuide(serverCount: 1, clientCount: 0, hasActivity: false)), .server)
        XCTAssertNil(firsts.observe(FirstRunGuide(serverCount: 2, clientCount: 0, hasActivity: false)))
        // What was marked is remembered across launches.
        firsts = FirstMoments(stored: firsts.stored)
        XCTAssertNil(firsts.observe(FirstRunGuide(serverCount: 2, clientCount: 0, hasActivity: false)))
        // Two at once say the later one, and neither comes back.
        XCTAssertEqual(firsts.observe(FirstRunGuide(serverCount: 2, clientCount: 1, hasActivity: true)), .activity)
        XCTAssertNil(firsts.observe(FirstRunGuide(serverCount: 2, clientCount: 1, hasActivity: true)))
        // Removing the only server and adding one again is not a first.
        XCTAssertNil(firsts.observe(FirstRunGuide(serverCount: 0, clientCount: 1, hasActivity: true)))
        XCTAssertNil(firsts.observe(FirstRunGuide(serverCount: 1, clientCount: 1, hasActivity: true)))
    }

    func testNothingIsMarkedOnAMacThatAlreadyHadServers() {
        var firsts = FirstMoments(stored: nil)
        XCTAssertNil(firsts.observe(FirstRunGuide(serverCount: 3, clientCount: 0, hasActivity: false)))
        XCTAssertNil(firsts.observe(FirstRunGuide(serverCount: 3, clientCount: 1, hasActivity: true)))
    }

    func testAClientLinkedBeforeAnyServerIsNotMarkedLater() {
        var firsts = FirstMoments(stored: nil)
        XCTAssertNil(firsts.observe(FirstRunGuide(serverCount: 0, clientCount: 1, hasActivity: false)))
        XCTAssertEqual(firsts.observe(FirstRunGuide(serverCount: 1, clientCount: 1, hasActivity: false)), .server)
    }

    func testTheAgentPromptShipsInTheApp() throws {
        let prompt = try XCTUnwrap(FirstRunGuide.agentPrompt)
        XCTAssertTrue(prompt.contains("plug import --dry-run"))
        XCTAssertTrue(prompt.contains("plug link --yes"))
    }
}
