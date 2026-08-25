import Foundation
import ServiceManagement

@MainActor
final class DaemonServiceManager {
    static let shared = DaemonServiceManager()
    private let agent = SMAppService.agent(plistName: "com.plug.daemon.plist")
    private let legacyPlist = FileManager.default.homeDirectoryForCurrentUser
        .appending(path: "Library/LaunchAgents/com.plug.daemon.plist")

    var status: SMAppService.Status { agent.status }
    var needsAdoption: Bool {
        guard agent.status == .enabled else { return true }
        return !Self.isAppManaged(
            launchctlOutput: launchctlPrint(),
            bundlePath: Bundle.main.bundlePath
        )
    }

    func adopt() throws {
        bootOutLegacyAgent()
        try? FileManager.default.removeItem(at: legacyPlist)
        if agent.status == .enabled { try? agent.unregister() }
        if agent.status != .enabled { try agent.register() }
        guard agent.status == .enabled else {
            openLoginItemSettings()
            throw CocoaError(.userCancelled, userInfo: [
                NSLocalizedDescriptionKey: "Allow Plug in Login Items, then choose Use Plug again."
            ])
        }
        stopExistingDaemon()
        kickstartAgent()
    }

    func restart() async throws {
        if agent.status == .enabled {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
                agent.unregister { error in
                    if let error { continuation.resume(throwing: error) }
                    else { continuation.resume() }
                }
            }
        }
        try agent.register()
    }

    func setMainAppAtLogin(_ enabled: Bool) throws {
        let service = SMAppService.mainApp
        if enabled, service.status != .enabled { try service.register() }
        if !enabled, service.status == .enabled { try service.unregister() }
    }

    func openLoginItemSettings() { SMAppService.openSystemSettingsLoginItems() }

    private func bootOutLegacyAgent() {
        runLaunchctl(["bootout", "gui/\(getuid())/com.plug.daemon"])
    }

    private func kickstartAgent() {
        runLaunchctl(["kickstart", "-k", "gui/\(getuid())/com.plug.daemon"])
    }

    private func launchctlPrint() -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        process.arguments = ["print", "gui/\(getuid())/com.plug.daemon"]
        let output = Pipe()
        process.standardOutput = output
        process.standardError = FileHandle.nullDevice
        guard (try? process.run()) != nil else { return "" }
        process.waitUntilExit()
        return String(
            data: output.fileHandleForReading.readDataToEndOfFile(),
            encoding: .utf8
        ) ?? ""
    }

    private func runLaunchctl(_ arguments: [String]) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        process.arguments = arguments
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try? process.run()
        process.waitUntilExit()
    }

    private func stopExistingDaemon() {
        guard let plug = Bundle.main.url(forResource: "plug", withExtension: nil) else { return }
        let process = Process()
        process.executableURL = plug
        process.arguments = ["stop"]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try? process.run()
        process.waitUntilExit()
    }

    static func isAppManaged(launchctlOutput: String, bundlePath: String) -> Bool {
        launchctlOutput.contains("BundleProgram") || launchctlOutput.contains(bundlePath)
    }
}
