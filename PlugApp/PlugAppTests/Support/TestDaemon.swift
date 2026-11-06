import Foundation
@testable import Plug
import PlugIPC

/// A handshake as the daemon sends one (`testdata/ipc/operator_handshake_response.json`),
/// decoded the way the app decodes it.
func makeTestHandshake(
    version: String,
    executable: URL? = URL(fileURLWithPath: "/Applications/Plug.app/Contents/Resources/plug"),
    ownership: String = OperatorHandshake.appManaged
) -> OperatorHandshake {
    var object: [String: Any] = [
        "daemon_version": version,
        "ipc_min": 3,
        "ipc_max": 6,
        "ownership": ownership,
        "capabilities": ["server_mutation", "server_config_read"],
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
