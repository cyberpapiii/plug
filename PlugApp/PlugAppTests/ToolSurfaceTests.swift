import XCTest
import PlugIPC
@testable import Plug

/// The tool list is the surface where someone switches a single capability off,
/// so these pin what a search finds, how a name reads, and which tools are
/// honestly out of reach because a wildcard covers them.
final class ToolCatalogTests: XCTestCase {
    private let catalog = ToolCatalog([
        ToolFacts(name: "figma__get_file", server: "figma", summary: "Read a design file"),
        ToolFacts(name: "figma__export_frame", server: "figma", summary: "Export an image"),
        ToolFacts(
            name: "notion__search",
            server: "notion",
            summary: "Search pages",
            isOn: false
        ),
        ToolFacts(
            name: "notion__append_block",
            server: "notion",
            isOn: false,
            lockedByPattern: "notion__append*"
        ),
    ])

    func testShortNameDropsTheServerPrefixTheGroupAlreadyStates() {
        XCTAssertEqual(
            ToolFacts(name: "figma__get_file", server: "figma").shortName,
            "get_file"
        )
    }

    func testShortNameKeepsNamesThatDoNotCarryThePrefix() {
        XCTAssertEqual(ToolFacts(name: "search", server: "notion").shortName, "search")
    }

    func testShortNameMatchesThePrefixRegardlessOfCase() {
        XCTAssertEqual(
            ToolFacts(name: "Figma__Get_File", server: "figma").shortName,
            "Get_File"
        )
    }

    func testGroupsAreServerAlphabeticalAndToolAlphabetical() {
        let groups = catalog.groups()
        XCTAssertEqual(groups.map(\.server), ["figma", "notion"])
        XCTAssertEqual(groups[0].tools.map(\.shortName), ["export_frame", "get_file"])
    }

    func testSearchingForAServerShowsEverythingThatServerCanDo() {
        let groups = catalog.groups(matching: "figma")
        XCTAssertEqual(groups.count, 1)
        XCTAssertEqual(groups[0].tools.count, 2)
    }

    func testSearchingMatchesDescriptionsNotJustNames() {
        let groups = catalog.groups(matching: "export an image")
        XCTAssertEqual(groups.flatMap(\.tools).map(\.name), ["figma__export_frame"])
    }

    func testSearchIgnoresSurroundingSpaceAndCase() {
        XCTAssertEqual(catalog.groups(matching: "  NOTION "), catalog.groups(matching: "notion"))
    }

    func testCountsSeparateOnFromOff() {
        XCTAssertEqual(catalog.onCount, 2)
        XCTAssertEqual(catalog.offCount, 2)
    }

    func testAGroupWithNothingLeftOnSaysSo() {
        let notion = catalog.groups(matching: "notion")[0]
        XCTAssertEqual(notion.onCount, 0)
        XCTAssertTrue(notion.isFullyOff)
        XCTAssertFalse(catalog.groups(matching: "figma")[0].isFullyOff)
    }

    func testAToolCoveredByAWildcardCannotBeSwitchedBackOnAlone() {
        let covered = catalog.tools(for: "notion").first { $0.lockedByPattern != nil }
        XCTAssertEqual(covered?.lockedByPattern, "notion__append*")
        XCTAssertEqual(covered?.canToggle, false)
        XCTAssertEqual(catalog.tools(for: "notion").first { $0.name.hasSuffix("search") }?.canToggle, true)
    }

    func testDaemonToolsBecomeFactsWithoutLosingWhySomethingIsOff() {
        let facts = ToolFacts(
            ToolInfo(
                name: "notion__append_block",
                serverId: "notion",
                title: "Append block",
                disabled: true,
                disabledByPattern: "notion__append*"
            )
        )
        XCTAssertEqual(facts.server, "notion")
        XCTAssertEqual(facts.summary, "Append block")
        XCTAssertFalse(facts.isOn)
        XCTAssertEqual(facts.lockedByPattern, "notion__append*")
    }

    func testDescriptionIsPreferredOverTitleWhenBothArrive() {
        let facts = ToolFacts(
            ToolInfo(name: "figma__get_file", serverId: "figma", description: "Read a design file", title: "Get file")
        )
        XCTAssertEqual(facts.summary, "Read a design file")
        XCTAssertTrue(facts.isOn)
        XCTAssertNil(facts.lockedByPattern)
    }

    func testPatternListsEveryToolItCovers() {
        let catalog = ToolCatalog([
            ToolFacts(name: "notion__search", server: "notion", isOn: false, lockedByPattern: "notion__*"),
            ToolFacts(name: "notion__append", server: "notion", isOn: false, lockedByPattern: "notion__*"),
            ToolFacts(name: "figma__get_file", server: "figma"),
            ToolFacts(name: "figma__delete", server: "figma", isOn: false),
        ])
        XCTAssertEqual(
            catalog.tools(coveredBy: "notion__*").map(\.shortName),
            ["append", "search"]
        )
        XCTAssertTrue(catalog.tools(coveredBy: "figma__*").isEmpty)
    }
}

/// The app list comes from `plug clients --output json`. These pin the shape
/// that command actually prints, including the fields it omits.
final class LinkableAppTests: XCTestCase {
    private func decode(_ json: String) throws -> [LinkableApp] {
        struct Listing: Decodable { let clients: [LinkableApp] }
        return try JSONDecoder().decode(Listing.self, from: Data(json.utf8)).clients
    }

    func testDecodesTheListingTheCommandPrints() throws {
        let apps = try decode(
            """
            {"clients":[{"target":"claude-desktop","name":"Claude Desktop","linked":true,
            "detected":true,"live":true,"live_sessions":2,"linked_transport":"stdio"}]}
            """
        )
        XCTAssertEqual(apps.count, 1)
        XCTAssertEqual(apps[0].id, "claude-desktop")
        XCTAssertEqual(apps[0].name, "Claude Desktop")
        XCTAssertTrue(apps[0].linked)
        XCTAssertTrue(apps[0].detected)
        XCTAssertTrue(apps[0].live)
        XCTAssertEqual(apps[0].sessions, 2)
        XCTAssertEqual(apps[0].transport, "stdio")
    }

    func testAnAppWithOnlyATargetStillDecodes() throws {
        let apps = try decode(#"{"clients":[{"target":"cursor"}]}"#)
        XCTAssertEqual(apps[0].name, "cursor")
        XCTAssertFalse(apps[0].linked)
        XCTAssertFalse(apps[0].detected)
        XCTAssertFalse(apps[0].live)
        XCTAssertEqual(apps[0].sessions, 0)
        XCTAssertNil(apps[0].transport)
    }

    func testAnEmptyListingIsNotAnError() throws {
        XCTAssertTrue(try decode(#"{"clients":[]}"#).isEmpty)
    }
}

/// Environment variables are typed by hand into one field, so the parser has to
/// accept the shapes people type.
final class EditServerEnvironmentTests: XCTestCase {
    func testParsesOnePairPerLine() {
        XCTAssertEqual(
            EditServerView.parseEnvironment("API_KEY=abc\nREGION=us-east-1"),
            ["API_KEY": "abc", "REGION": "us-east-1"]
        )
    }

    func testCommasStayInsideValues() {
        XCTAssertEqual(
            EditServerView.parseEnvironment("A=1, B=2"),
            ["A": "1, B=2"]
        )
    }

    func testSpaceAroundTheNameAndValueIsTrimmed() {
        XCTAssertEqual(EditServerView.parseEnvironment("  TOKEN = xyz  "), ["TOKEN": "xyz"])
    }

    func testValuesKeepTheirOwnEqualsSigns() {
        XCTAssertEqual(EditServerView.parseEnvironment("URL=a=b=c"), ["URL": "a=b=c"])
    }

    func testAnEmptyValueIsKept() {
        XCTAssertEqual(EditServerView.parseEnvironment("EMPTY="), ["EMPTY": ""])
    }

    func testLinesWithoutAnEqualsOrANameAreSkipped() {
        XCTAssertEqual(EditServerView.parseEnvironment("nonsense\n=value\n\nA=1"), ["A": "1"])
    }

    func testNothingTypedMeansNoEnvironment() {
        XCTAssertTrue(EditServerView.parseEnvironment("   \n ").isEmpty)
    }

    func testSaveStaysDisabledWithoutServerConfigRead() {
        XCTAssertFalse(
            EditServerView.canSave(
                canReadServerConfig: false,
                loaded: true,
                isComplete: true,
                saving: false
            )
        )
        XCTAssertTrue(
            EditServerView.canSave(
                canReadServerConfig: true,
                loaded: true,
                isComplete: true,
                saving: false
            )
        )
    }

    func testMissingCapabilityCopyIsRestartUpdateNotParseError() {
        XCTAssertEqual(
            EditServerView.missingCapabilityCopy,
            AppModel.serverConfigReadRequiredCopy
        )
        XCTAssertTrue(EditServerView.missingCapabilityCopy.contains("Restart"))
        XCTAssertTrue(EditServerView.missingCapabilityCopy.contains("update"))
        XCTAssertFalse(EditServerView.missingCapabilityCopy.contains("PARSE_ERROR"))
        XCTAssertFalse(EditServerView.missingCapabilityCopy.contains("could not be loaded"))
    }

    func testArgumentsRoundTripThroughTheDisplayedCommandLine() {
        let arguments = ["-y", "a package", "", "it's-safe", #"a\"quote"#]
        XCTAssertEqual(
            ServerDraftParser.tokenize(EditServerView.renderArguments(arguments)),
            arguments
        )
    }
}
