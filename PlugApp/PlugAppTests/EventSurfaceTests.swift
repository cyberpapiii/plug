import XCTest
import PlugIPC
@testable import Plug

/// The Events tab says how a watch is doing in one sentence and builds the
/// watch the daemon saves, so these pin the wording and what is sent.
final class EventSurfaceTests: XCTestCase {
    func testAWatchSaysWhereItComesFromAndHowItIsDoing() {
        let event = EventFacts(EventStatus(
            name: "gmail.unread", server: "gmail", tool: "search_messages", everySecs: 300,
            state: "watching", lastChecked: 1_000, lastChanged: 400, subscribers: 1
        ))
        XCTAssertEqual(event.source, "search_messages on gmail, every 5 minutes")
        XCTAssertEqual(event.healthLine(now: 1_030), "Checked just now; last change 10 min ago.")
        XCTAssertEqual(event.listenerLine, "1 client is listening")
        XCTAssertFalse(event.health.needsAttention)
        XCTAssertTrue(event.canRemove)
    }

    func testAWatchInTroubleSaysWhy() {
        let missing = EventFacts(EventStatus(name: "a.b", server: "a", tool: "t", state: "tool_missing"))
        XCTAssertTrue(missing.health.needsAttention)
        XCTAssertEqual(missing.healthLine(now: 0), "The tool is not there right now. Is the server running?")
        let failed = EventFacts(EventStatus(name: "a.b", server: "a", tool: "t", state: "call_failed", lastChecked: 0))
        XCTAssertEqual(failed.healthLine(now: 7_200), "The last check failed (2 hr ago). Plug keeps trying.")
        // A state this build has not heard of reads as a watch still waiting.
        XCTAssertEqual(EventFacts.Health(state: "something_new"), .waiting)
    }

    func testAnEventPlugDoesNotWatchCannotBeStoppedHere() {
        let event = EventFacts(EventStatus(name: "slack.ditto_message", server: "slack"))
        XCTAssertFalse(event.canRemove)
        XCTAssertEqual(event.source, "Sent by slack")
        XCTAssertEqual(event.listenerLine, "Nobody is listening")
    }

    func testIntervalsAndAgesReadAsWords() {
        XCTAssertEqual(EventFacts.interval(60), "every minute")
        XCTAssertEqual(EventFacts.interval(900), "every 15 minutes")
        XCTAssertEqual(EventFacts.interval(3600), "every hour")
        XCTAssertEqual(EventFacts.interval(7200), "every 2 hours")
        XCTAssertEqual(EventFacts.interval(45), "every 45 seconds")
        XCTAssertEqual(EventFacts.ago(nil, now: 10), "never")
        XCTAssertEqual(EventFacts.ago(0, now: 86_400), "1 day ago")
        // A clock that moved backwards does not underflow.
        XCTAssertEqual(EventFacts.ago(50, now: 10), "just now")
    }

    func testTheEventNameComesFromTheToolName() {
        XCTAssertEqual(WatchDraft.name(fromTool: "search_messages"), "search_messages")
        XCTAssertEqual(WatchDraft.name(fromTool: "List-Open Issues!"), "list_open_issues")
        XCTAssertEqual(WatchDraft.name(fromTool: ""), "")
    }

    func testArgumentsAreOneJSONObjectOrNothing() {
        XCTAssertEqual(WatchDraft.arguments(from: "  \n"), .success([:]))
        XCTAssertEqual(
            WatchDraft.arguments(from: #"{"query":"is:unread","maxResults":5,"deep":true,"tags":["a"]}"#),
            .success([
                "query": .string("is:unread"), "maxResults": .number(5),
                "deep": .bool(true), "tags": .array([.string("a")]),
            ])
        )
        XCTAssertEqual(WatchDraft.arguments(from: "[1, 2]"), .failure(.notAnObject))
        XCTAssertEqual(WatchDraft.arguments(from: "query=is:unread"), .failure(.notAnObject))
    }

    func testAToolCarriesItsOwnNameAndWhetherItIsReadOnly() {
        let tool = ToolFacts(ToolInfo(
            name: "Gmail__search_messages", serverId: "workspace",
            ownName: "search_gmail_messages", readOnly: true
        ))
        XCTAssertEqual(tool.ownName, "search_gmail_messages")
        XCTAssertTrue(tool.isReadOnly)
    }
}
