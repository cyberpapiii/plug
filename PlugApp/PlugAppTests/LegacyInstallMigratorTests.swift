import Foundation
import XCTest
@testable import Plug

final class LegacyInstallMigratorTests: XCTestCase {
    func testInspectsAbsentCanonicalBrokenAndUnrelatedShellLinks() async throws {
        let fixture = try Fixture()
        let canonical = fixture.canonical

        var snapshot = try await fixture.migrator.inspect(canonical: canonical)
        XCTAssertEqual(snapshot.shellLink, .absent)

        try fixture.createSymlink(at: fixture.shellLink, target: canonical.executableURL)
        snapshot = try await fixture.migrator.inspect(canonical: canonical)
        XCTAssertEqual(snapshot.shellLink, .canonical(canonical.executableURL))

        try FileManager.default.removeItem(at: fixture.shellLink)
        let missing = fixture.root.appending(path: "missing/plug")
        try fixture.createSymlink(at: fixture.shellLink, target: missing)
        snapshot = try await fixture.migrator.inspect(canonical: canonical)
        XCTAssertEqual(snapshot.shellLink, .repairable(missing))

        try FileManager.default.removeItem(at: fixture.shellLink)
        try Data("do not replace".utf8).write(to: fixture.shellLink)
        snapshot = try await fixture.migrator.inspect(canonical: canonical)
        XCTAssertEqual(snapshot.shellLink, .unrelated(fixture.shellLink))
        XCTAssertTrue(snapshot.unknownPaths.contains(fixture.shellLink))
    }

    func testRepairsBrokenShellLinkAtomicallyButRefusesRegularFile() async throws {
        let fixture = try Fixture()
        let missing = fixture.root.appending(path: "missing/plug")
        try fixture.createSymlink(at: fixture.shellLink, target: missing)

        let repaired = try await fixture.migrator.repairShellLink(to: fixture.canonical.executableURL)
        XCTAssertEqual(repaired, .canonical(fixture.canonical.executableURL))
        XCTAssertEqual(try FileManager.default.destinationOfSymbolicLink(atPath: fixture.shellLink.path), fixture.canonical.executableURL.path)
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: fixture.shellLink.deletingLastPathComponent().path).sorted(), ["plug"])

        try FileManager.default.removeItem(at: fixture.shellLink)
        try Data("mine".utf8).write(to: fixture.shellLink)
        do {
            _ = try await fixture.migrator.repairShellLink(to: fixture.canonical.executableURL)
            XCTFail("Expected unrelated-file refusal")
        } catch {
            XCTAssertEqual(error as? LegacyInstallError, .unrelatedShellCommand(fixture.shellLink))
        }
        XCTAssertEqual(try String(contentsOf: fixture.shellLink, encoding: .utf8), "mine")
    }

    func testRemovesFormulaOnlyThroughTrustedBrewCommand() async throws {
        let runner = RecordingProcessRunner { executable, arguments in
            if arguments.first == "list" {
                return ProcessResult(status: 0, stdout: Data("plug 0.6.4\n".utf8), stderr: Data())
            }
            return ProcessResult(status: 0, stdout: Data(), stderr: Data())
        }
        let fixture = try Fixture(runner: runner)
        let snapshot = try await fixture.migrator.inspect(canonical: fixture.canonical)

        XCTAssertTrue(snapshot.formulaInstalled)
        try await fixture.migrator.removeRecognizedFormula(snapshot)
        let calls = await runner.calls
        XCTAssertEqual(calls.last?.executable, fixture.brew)
        XCTAssertEqual(calls.last?.arguments, ["uninstall", "cyberpapiii/tap/plug"])
    }

    func testFailedBrewUninstallPreservesLegacyFiles() async throws {
        let runner = RecordingProcessRunner { _, arguments in
            if arguments.first == "list" {
                return ProcessResult(status: 0, stdout: Data("plug 0.6.4\n".utf8), stderr: Data())
            }
            return ProcessResult(status: 1, stdout: Data(), stderr: Data("busy".utf8))
        }
        let fixture = try Fixture(runner: runner)
        try Data("cargo".utf8).write(to: fixture.cargo)
        let snapshot = try await fixture.migrator.inspect(canonical: fixture.canonical)

        do {
            try await fixture.migrator.removeRecognizedFormula(snapshot)
            XCTFail("Expected brew failure")
        } catch {
            XCTAssertEqual(error as? LegacyInstallError, .brewFailed("busy"))
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.cargo.path))
    }

    func testFormulaMustBeRemovedBeforeShellLinkRepair() async throws {
        let runner = RecordingProcessRunner { _, arguments in
            if arguments.first == "list" {
                return ProcessResult(status: 0, stdout: Data("plug 0.6.4\n".utf8), stderr: Data())
            }
            return ProcessResult(status: 0, stdout: Data(), stderr: Data())
        }
        let fixture = try Fixture(runner: runner)

        do {
            _ = try await fixture.migrator.repairShellLink(to: fixture.canonical.executableURL)
            XCTFail("Expected formula ordering refusal")
        } catch {
            XCTAssertEqual(error as? LegacyInstallError, .formulaStillInstalled)
        }
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.shellLink.path))
    }

    func testCargoBinaryIsRetainedUntilExactProofThenDeleted() async throws {
        let runner = RecordingProcessRunner { _, arguments in
            if arguments == ["--version"] {
                return ProcessResult(status: 0, stdout: Data("plug 0.7.0\n".utf8), stderr: Data())
            }
            return ProcessResult(status: 1, stdout: Data(), stderr: Data())
        }
        let fixture = try Fixture(runner: runner)
        try Data("legacy plug".utf8).write(to: fixture.cargo)
        _ = try await fixture.migrator.repairShellLink(to: fixture.canonical.executableURL)
        let snapshot = try await fixture.migrator.inspect(canonical: fixture.canonical)
        XCTAssertEqual(snapshot.cargoBinary, fixture.cargo)

        try await fixture.migrator.removeVerifiedCargoBinary(
            snapshot,
            proof: ReconciliationProof(
                appVersion: "0.7.0",
                embeddedVersion: "0.7.0",
                daemonVersion: "0.6.4",
                shellTarget: fixture.canonical.executableURL,
                appManaged: true
            )
        )
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.cargo.path))

        try await fixture.migrator.removeVerifiedCargoBinary(
            snapshot,
            proof: ReconciliationProof(
                appVersion: "0.7.0",
                embeddedVersion: "0.7.0",
                daemonVersion: "0.7.0",
                shellTarget: URL(fileURLWithPath: "/tmp/plug"),
                appManaged: true
            )
        )
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.cargo.path))

        try await fixture.migrator.removeVerifiedCargoBinary(
            snapshot,
            proof: ReconciliationProof(
                appVersion: "0.7.0",
                embeddedVersion: "0.7.0",
                daemonVersion: "0.7.0",
                shellTarget: fixture.canonical.executableURL,
                appManaged: true
            )
        )
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.cargo.path))
    }

    func testCargoReplacementAfterInspectionIsNeverDeleted() async throws {
        let runner = RecordingProcessRunner { executable, arguments in
            guard arguments == ["--version"],
                  let contents = try? String(contentsOf: executable, encoding: .utf8),
                  contents == "legacy plug"
            else {
                return ProcessResult(status: 1, stdout: Data(), stderr: Data())
            }
            return ProcessResult(status: 0, stdout: Data("plug 0.7.0\n".utf8), stderr: Data())
        }
        let fixture = try Fixture(runner: runner)
        try Data("legacy plug".utf8).write(to: fixture.cargo)
        _ = try await fixture.migrator.repairShellLink(to: fixture.canonical.executableURL)
        let snapshot = try await fixture.migrator.inspect(canonical: fixture.canonical)
        try Data("unknown replacement".utf8).write(to: fixture.cargo)

        try await fixture.migrator.removeVerifiedCargoBinary(
            snapshot,
            proof: ReconciliationProof(
                appVersion: "0.7.0",
                embeddedVersion: "0.7.0",
                daemonVersion: "0.7.0",
                shellTarget: fixture.canonical.executableURL,
                appManaged: true
            )
        )

        XCTAssertEqual(try String(contentsOf: fixture.cargo, encoding: .utf8), "unknown replacement")
    }
}

private actor RecordingProcessRunner: ProcessRunning {
    struct Call: Sendable {
        let executable: URL
        let arguments: [String]
    }

    private(set) var calls: [Call] = []
    private let handler: @Sendable (URL, [String]) -> ProcessResult

    init(handler: @escaping @Sendable (URL, [String]) -> ProcessResult) {
        self.handler = handler
    }

    func run(executable: URL, arguments: [String], timeout: Duration) async throws -> ProcessResult {
        calls.append(Call(executable: executable, arguments: arguments))
        return handler(executable, arguments)
    }
}

private final class Fixture {
    let root: URL
    let home: URL
    let shellLink: URL
    let cargo: URL
    let brew: URL
    let canonical: VerifiedAppInstallation
    let migrator: LegacyInstallMigrator

    init(runner: any ProcessRunning = RecordingProcessRunner { _, _ in
        ProcessResult(status: 1, stdout: Data(), stderr: Data())
    }) throws {
        root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        home = root.appending(path: "home")
        shellLink = home.appending(path: ".local/bin/plug")
        cargo = home.appending(path: ".cargo/bin/plug")
        brew = root.appending(path: "trusted/brew")
        let app = root.appending(path: "Applications/Plug.app")
        let executable = app.appending(path: "Contents/Resources/plug")
        canonical = VerifiedAppInstallation(
            bundleURL: app,
            executableURL: executable,
            appVersion: "0.7.0",
            buildVersion: "20",
            embeddedVersion: "0.7.0",
            teamID: "HJF7LN64XX"
        )
        try FileManager.default.createDirectory(at: shellLink.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: cargo.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: executable.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: brew.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("canonical".utf8).write(to: executable)
        try Data("brew".utf8).write(to: brew)
        migrator = LegacyInstallMigrator(homeURL: home, brewURLs: [brew], runner: runner)
    }

    deinit {
        try? FileManager.default.removeItem(at: root)
    }

    func createSymlink(at url: URL, target: URL) throws {
        try FileManager.default.createSymbolicLink(atPath: url.path, withDestinationPath: target.path)
    }
}
