import Foundation
import ServiceManagement

@MainActor
final class DaemonServiceManager {
    static let shared = DaemonServiceManager()
    private let agent = SMAppService.agent(plistName: "com.plug.daemon.plist")
    private let legacyPlist = FileManager.default.homeDirectoryForCurrentUser
        .appending(path: "Library/LaunchAgents/com.plug.daemon.plist")

    var status: SMAppService.Status { agent.status }
    var needsAdoption: Bool { agent.status != .enabled }

    func adopt() throws {
        bootOutLegacyAgent()
        try? FileManager.default.removeItem(at: legacyPlist)
        if agent.status == .enabled { try agent.unregister() }
        try agent.register()
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
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        process.arguments = [
            "bootout",
            "gui/\(getuid())/com.plug.daemon",
        ]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try? process.run()
        process.waitUntilExit()
    }
}
