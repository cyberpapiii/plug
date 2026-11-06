import Foundation
import PlugIPC
import XCTest

extension XCTestCase {
    /// A Plug folder of this test's own, holding a daemon token, removed when
    /// the test ends. Under /tmp so a socket inside it fits `sun_path`.
    func makeTestHome(token: String? = "fixture-token") -> PlugHome {
        let directory = URL(
            fileURLWithPath: "/tmp/plug-home-\(UUID().uuidString.prefix(8))", isDirectory: true
        )
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let home = PlugHome(directory: directory)
        if let token { try? token.write(to: home.tokenURL, atomically: true, encoding: .utf8) }
        addTeardownBlock { try? FileManager.default.removeItem(at: directory) }
        return home
    }

    /// Preferences of this test's own, never the host app's.
    func makeTestDefaults() -> UserDefaults {
        let suite = "PlugTests-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        addTeardownBlock { UserDefaults().removePersistentDomain(forName: suite) }
        return defaults
    }
}
