import Foundation
import XCTest
@testable import Plug

final class LegacyInstallMigratorTests: XCTestCase {
    func testInspectsAbsentCanonicalBrokenAndUnrelatedShellLinks() async throws {
        let fixture = try Fixture()
        let canonical = fixture.canonical

        var snapshot = try await fixture.migrator.inspect(canonical: canonical)
        XCTAssertEqual(snapshot.shellLink, .absent)
        XCTAssertFalse(snapshot.recognizedPaths.contains(fixture.shellLink))

        try fixture.createSymlink(at: fixture.shellLink, target: canonical.executableURL)
        snapshot = try await fixture.migrator.inspect(canonical: canonical)
        XCTAssertEqual(snapshot.shellLink, .canonical(canonical.executableURL))
        XCTAssertTrue(snapshot.recognizedPaths.contains(fixture.shellLink))

        try FileManager.default.removeItem(at: fixture.shellLink)
        let missing = fixture.root.appending(path: "missing/plug")
        try fixture.createSymlink(at: fixture.shellLink, target: missing)
        snapshot = try await fixture.migrator.inspect(canonical: canonical)
        XCTAssertEqual(snapshot.shellLink, .repairable(missing))
        XCTAssertTrue(snapshot.recognizedPaths.contains(fixture.shellLink))

        try FileManager.default.removeItem(at: fixture.shellLink)
        try Data("do not replace".utf8).write(to: fixture.shellLink)
        snapshot = try await fixture.migrator.inspect(canonical: canonical)
        XCTAssertEqual(snapshot.shellLink, .unrelated(fixture.shellLink))
        XCTAssertTrue(snapshot.unknownPaths.contains(fixture.shellLink))
        XCTAssertFalse(snapshot.recognizedPaths.contains(fixture.shellLink))
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
        let runner = StubProcessRunner { executable, arguments in
            if arguments.first == "list" {
                return ProcessResult(status: 0, stdout: Data("plug 0.6.4\n".utf8), stderr: Data())
            }
            return ProcessResult(status: 0, stdout: Data(), stderr: Data())
        }
        let fixture = try Fixture(runner: runner, formulaKeg: true)
        let snapshot = try await fixture.migrator.inspect(canonical: fixture.canonical)

        XCTAssertTrue(snapshot.formulaInstalled)
        try await fixture.migrator.removeRecognizedFormula(snapshot)
        let calls = await runner.calls
        XCTAssertEqual(calls.last?.executable, fixture.brew)
        XCTAssertEqual(calls.last?.arguments, ["uninstall", "cyberpapiii/tap/plug"])
    }

    func testFailedBrewUninstallPreservesLegacyFiles() async throws {
        let runner = StubProcessRunner { _, arguments in
            if arguments.first == "list" {
                return ProcessResult(status: 0, stdout: Data("plug 0.6.4\n".utf8), stderr: Data())
            }
            return ProcessResult(status: 1, stdout: Data(), stderr: Data("busy".utf8))
        }
        let fixture = try Fixture(runner: runner, formulaKeg: true)
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

    /// Without a keg under brew's prefix there is no formula, so brew is not
    /// run at all. Each call costs about a second, more at login.
    func testBrewIsNotAskedWhenNoFormulaKegExists() async throws {
        let runner = StubProcessRunner { _, _ in
            ProcessResult(status: 0, stdout: Data("plug 0.6.4\n".utf8), stderr: Data())
        }
        let fixture = try Fixture(runner: runner)

        let snapshot = try await fixture.migrator.inspect(canonical: fixture.canonical)
        _ = try await fixture.migrator.repairShellLink(to: fixture.canonical.executableURL)

        XCTAssertFalse(snapshot.formulaInstalled)
        let calls = await runner.calls
        XCTAssertTrue(calls.isEmpty, "brew ran \(calls.count) times with no keg on disk")
    }

    /// A keg reached through `opt/plug` alone still sends the question to brew.
    func testOptLinkAloneStillAsksBrew() async throws {
        let runner = StubProcessRunner { _, arguments in
            ProcessResult(
                status: 0,
                stdout: Data(arguments.first == "list" ? "plug 0.6.4\n".utf8 : "".utf8),
                stderr: Data()
            )
        }
        let fixture = try Fixture(runner: runner)
        try FileManager.default.createDirectory(
            at: fixture.root.appending(path: "opt"),
            withIntermediateDirectories: true
        )
        try fixture.createSymlink(
            at: fixture.root.appending(path: "opt/plug"),
            target: fixture.root.appending(path: "missing-keg")
        )

        let snapshot = try await fixture.migrator.inspect(canonical: fixture.canonical)

        XCTAssertTrue(snapshot.formulaInstalled)
    }

    func testFormulaMustBeRemovedBeforeShellLinkRepair() async throws {
        let runner = StubProcessRunner { _, arguments in
            if arguments.first == "list" {
                return ProcessResult(status: 0, stdout: Data("plug 0.6.4\n".utf8), stderr: Data())
            }
            return ProcessResult(status: 0, stdout: Data(), stderr: Data())
        }
        let fixture = try Fixture(runner: runner, formulaKeg: true)

        do {
            _ = try await fixture.migrator.repairShellLink(to: fixture.canonical.executableURL)
            XCTFail("Expected formula ordering refusal")
        } catch {
            XCTAssertEqual(error as? LegacyInstallError, .formulaStillInstalled)
        }
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.shellLink.path))
    }

    func testUnprovenCargoBinaryIsNeverExecutedOrDeleted() async throws {
        let runner = StubProcessRunner { _, _ in
            ProcessResult(status: 0, stdout: Data("plug 0.7.0\n".utf8), stderr: Data())
        }
        let fixture = try Fixture(runner: runner)
        try Data("legacy plug".utf8).write(to: fixture.cargo)
        let snapshot = try await fixture.migrator.inspect(canonical: fixture.canonical)
        XCTAssertNil(snapshot.cargoBinary)
        XCTAssertTrue(snapshot.unknownPaths.contains(fixture.cargo))

        let calls = await runner.calls
        XCTAssertFalse(calls.contains { $0.executable.standardizedFileURL == fixture.cargo.standardizedFileURL })
    }

    func testCargoBinaryIsRetainedUntilExactProofThenDeleted() async throws {
        let identity = LegacyBinaryIdentity(
            sha256: "legacy-digest"
        )
        let fixture = try Fixture(
            identityReader: { url in
                guard (try? String(contentsOf: url, encoding: .utf8)) == "legacy plug" else { return nil }
                return identity
            }
        )
        try Data("legacy plug".utf8).write(to: fixture.cargo)
        _ = try await fixture.migrator.repairShellLink(to: fixture.canonical.executableURL)
        let snapshot = try await fixture.migrator.inspect(canonical: fixture.canonical)
        XCTAssertEqual(snapshot.cargoBinary, fixture.cargo)
        XCTAssertEqual(snapshot.cargoBinaryIdentity, identity)

        // A daemon of another version, or a shell command that runs something
        // else, proves nothing, so there is no proof to remove the copy with.
        XCTAssertNil(OwnDaemonProof(
            app: fixture.canonical,
            handshake: makeTestHandshake(version: "0.6.4", executable: fixture.canonical.executableURL),
            shellLink: .canonical(fixture.canonical.executableURL)
        ))
        XCTAssertNil(OwnDaemonProof(
            app: fixture.canonical,
            handshake: makeTestHandshake(version: "0.7.0", executable: fixture.canonical.executableURL),
            shellLink: .canonical(URL(fileURLWithPath: "/tmp/plug"))
        ))

        try await fixture.migrator.removeVerifiedCargoBinary(
            snapshot,
            proof: fixture.proof
        )
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.cargo.path))
    }

    func testCargoReplacementAfterInspectionIsNeverDeleted() async throws {
        let identity = LegacyBinaryIdentity(
            sha256: "legacy-digest"
        )
        let fixture = try Fixture(
            identityReader: { url in
                guard (try? String(contentsOf: url, encoding: .utf8)) == "legacy plug" else { return nil }
                return identity
            }
        )
        try Data("legacy plug".utf8).write(to: fixture.cargo)
        _ = try await fixture.migrator.repairShellLink(to: fixture.canonical.executableURL)
        let snapshot = try await fixture.migrator.inspect(canonical: fixture.canonical)
        try Data("unknown replacement".utf8).write(to: fixture.cargo)

        try await fixture.migrator.removeVerifiedCargoBinary(
            snapshot,
            proof: fixture.proof
        )

        XCTAssertEqual(try String(contentsOf: fixture.cargo, encoding: .utf8), "unknown replacement")
    }

    func testCargoDirectoryReplacementAfterIdentityCheckIsPreserved() async throws {
        let identity = LegacyBinaryIdentity(
            sha256: "legacy-digest"
        )
        let fixture = try Fixture(
            identityReader: { url in
                guard (try? String(contentsOf: url, encoding: .utf8)) == "legacy plug" else {
                    return nil
                }
                let trigger = url.deletingLastPathComponent().appending(path: ".replace-with-directory")
                guard FileManager.default.fileExists(atPath: trigger.path) else { return identity }

                try? FileManager.default.removeItem(at: url)
                try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: false)
                try? Data("do not delete".utf8).write(to: url.appending(path: "nested"))
                try? FileManager.default.removeItem(at: trigger)
                return identity
            }
        )
        try Data("legacy plug".utf8).write(to: fixture.cargo)
        _ = try await fixture.migrator.repairShellLink(to: fixture.canonical.executableURL)
        let snapshot = try await fixture.migrator.inspect(canonical: fixture.canonical)
        try Data().write(to: fixture.cargo.deletingLastPathComponent().appending(path: ".replace-with-directory"))

        do {
            try await fixture.migrator.removeVerifiedCargoBinary(
                snapshot,
                proof: fixture.proof
            )
            XCTFail("Expected replacement directory to fail closed")
        } catch let error as LegacyInstallError {
            guard case .fileOperation = error else {
                return XCTFail("Unexpected legacy error: \(error)")
            }
        }

        var isDirectory: ObjCBool = false
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.cargo.path, isDirectory: &isDirectory))
        XCTAssertTrue(isDirectory.boolValue)
        XCTAssertEqual(
            try String(contentsOf: fixture.cargo.appending(path: "nested"), encoding: .utf8),
            "do not delete"
        )
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
    /// The app's own daemon answering, with the shell command linked to it.
    var proof: OwnDaemonProof {
        OwnDaemonProof(
            app: canonical,
            handshake: makeTestHandshake(version: canonical.appVersion, executable: canonical.executableURL),
            shellLink: .canonical(canonical.executableURL)
        )!
    }

    init(
        runner: any ProcessRunning = StubProcessRunner { _, _ in
            ProcessResult(status: 1, stdout: Data(), stderr: Data())
        },
        identityReader: @escaping @Sendable (URL) -> LegacyBinaryIdentity? = { _ in nil },
        formulaKeg: Bool = false
    ) throws {
        root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        home = root.appending(path: "home")
        shellLink = home.appending(path: ".local/bin/plug")
        cargo = home.appending(path: ".cargo/bin/plug")
        brew = root.appending(path: "trusted/brew")
        let app = root.appending(path: "Applications/Plug.app")
        let executable = app.appending(path: "Contents/Resources/plug")
        canonical = makeTestInstallation(bundle: app)
        try FileManager.default.createDirectory(at: shellLink.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: cargo.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: executable.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: brew.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("canonical".utf8).write(to: executable)
        try Data("brew".utf8).write(to: brew)
        if formulaKeg {
            // What an installed formula leaves under the prefix above brew's bin.
            try FileManager.default.createDirectory(
                at: root.appending(path: "Cellar/plug/0.6.4"),
                withIntermediateDirectories: true
            )
        }
        migrator = LegacyInstallMigrator(
            homeURL: home,
            brewURLs: [brew],
            runner: runner,
            identityReader: identityReader
        )
    }

    deinit {
        try? FileManager.default.removeItem(at: root)
    }

    func createSymlink(at url: URL, target: URL) throws {
        try FileManager.default.createSymbolicLink(atPath: url.path, withDestinationPath: target.path)
    }
}
