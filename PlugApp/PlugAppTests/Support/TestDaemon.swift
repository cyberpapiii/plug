import Foundation
@testable import Plug
import PlugIPC

/// A handshake as the daemon sends one (`handshake` in
/// `testdata/ipc/responses.json`), decoded the way the app decodes it.
func makeTestHandshake(
    version: String,
    executable: URL? = URL(fileURLWithPath: "/Applications/Plug.app/Contents/Resources/plug"),
    ownership: DaemonOwnership = .appManaged
) -> OperatorHandshake {
    var object: [String: Any] = [
        "daemon_version": version,
        "ownership": ownership.rawValue,
    ]
    if let executable { object["daemon_executable"] = executable.path }
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    return try! decoder.decode(OperatorHandshake.self, from: JSONSerialization.data(withJSONObject: object))
}

/// The app as installed in /Applications, at one version and build.
func makeTestInstallation(
    version: String = "0.7.0",
    build: String = "20",
    bundle: URL = URL(fileURLWithPath: "/Applications/Plug.app")
) -> VerifiedAppInstallation {
    VerifiedAppInstallation(
        bundleURL: bundle,
        executableURL: bundle.appending(path: "Contents/Resources/plug"),
        appVersion: version,
        buildVersion: build,
        embeddedVersion: version
    )
}
