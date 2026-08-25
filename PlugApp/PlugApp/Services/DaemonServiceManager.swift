import Foundation
import ServiceManagement

@MainActor
final class DaemonServiceManager {
    static let shared = DaemonServiceManager()
    // ServiceManagement is process-safe and owns its own XPC serialization.
    // Its async methods are nonisolated, so keep this immutable handle usable
    // across that boundary under Swift 6's strict concurrency checks.
    nonisolated(unsafe) private let agent = SMAppService.agent(plistName: "com.plug.daemon.plist")
    private let legacyPlist = FileManager.default.homeDirectoryForCurrentUser
        .appending(path: "Library/LaunchAgents/com.plug.daemon.plist")

    var status: SMAppService.Status { agent.status }
    var needsAdoption: Bool {
        guard agent.status == .enabled else { return true }
        return !Self.isAppManaged(
            launchctlOutput: launchctlPrint(),
            bundleIdentifier: Bundle.main.bundleIdentifier ?? "",
            bundleVersion: Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "",
            bundlePath: Bundle.main.bundlePath
        )
    }

    func adopt() async throws {
        // Pause legacy connectors before removing their LaunchAgent. Otherwise
        // one can observe the missing socket in this handoff window and
        // recreate the CLI-owned service before SMAppService registers ours.
        let pausedConnectors = pauseLegacyConnectors()
        defer { resumeLegacyConnectors(pausedConnectors) }
        bootOutLegacyAgent()
        try? FileManager.default.removeItem(at: legacyPlist)
        if agent.status == .enabled { try? await unregisterAgent() }
        if agent.status != .enabled { try agent.register() }
        guard agent.status == .enabled else {
            openLoginItemSettings()
            throw CocoaError(.userCancelled, userInfo: [
                NSLocalizedDescriptionKey: "Allow Plug in Login Items, then choose Use Plug again."
            ])
        }
        try await replaceRunningDaemon()
    }

    func restart() async throws {
        if agent.status == .enabled {
            try await unregisterAgent()
        }
        try agent.register()
    }

    private func unregisterAgent() async throws {
        // The callback variant completes on a private ServiceManagement queue.
        // Calling it from this MainActor type makes Swift 6 enforce the actor
        // precondition on that queue and crash before the callback body runs.
        // The async API resumes through Swift concurrency safely; poll its
        // observable status before registering the replacement.
        try await agent.unregister()
        for _ in 0..<60 where agent.status == .enabled {
            try? await Task.sleep(for: .milliseconds(50))
        }
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

    private func replaceRunningDaemon() async throws {
        for _ in 0..<3 {
            let previousPID = daemonPID()
            stopExistingDaemon()
            if let previousPID {
                await waitForExit(pid: previousPID)
            }
            kickstartAgent()
            if await waitForAgentReady() { return }
            if launchctlPrint().contains("state = running") { break }
        }
        throw CocoaError(.executableLoad, userInfo: [
            NSLocalizedDescriptionKey: "Plug's background service did not become ready. Approve any Keychain prompt, then choose Use Plug again."
        ])
    }

    private func daemonPID() -> Int32? {
        let url = FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Library/Application Support/plug/plug.pid")
        guard let value = try? String(contentsOf: url, encoding: .utf8),
              let pid = Int32(value.trimmingCharacters(in: .whitespacesAndNewlines))
        else { return nil }
        return pid
    }

    private func waitForExit(pid: Int32) async {
        for _ in 0..<60 {
            if kill(pid, 0) != 0 { return }
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    private func waitForAgentReady() async -> Bool {
        for _ in 0..<120 {
            if launchctlPrint().contains("state = running"), daemonAvailable() {
                return true
            }
            try? await Task.sleep(for: .milliseconds(250))
        }
        return false
    }

    private func daemonAvailable() -> Bool {
        guard let plug = Bundle.main.url(forResource: "plug", withExtension: nil) else {
            return false
        }
        let process = Process()
        let pipe = Pipe()
        process.executableURL = plug
        process.arguments = ["status", "--output", "json"]
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            guard process.terminationStatus == 0,
                  let payload = try JSONSerialization.jsonObject(with: data) as? [String: Any]
            else { return false }
            return payload["daemon_running"] as? Bool == true
        } catch {
            return false
        }
    }

    private func pauseLegacyConnectors() -> [Int32] {
        Self.connectorPIDs(psOutput: currentUserProcessList()).compactMap { pid in
            guard kill(pid, SIGSTOP) == 0 else { return nil }
            return pid
        }
    }

    private func resumeLegacyConnectors(_ pids: [Int32]) {
        for pid in pids { _ = kill(pid, SIGCONT) }
    }

    private func currentUserProcessList() -> String {
        let process = Process()
        let pipe = Pipe()
        process.executableURL = URL(fileURLWithPath: "/bin/ps")
        process.arguments = ["-x", "-o", "pid=", "-o", "command="]
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            return String(decoding: data, as: UTF8.self)
        } catch {
            return ""
        }
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

    static func isAppManaged(
        launchctlOutput: String,
        bundleIdentifier: String,
        bundleVersion: String,
        bundlePath: String
    ) -> Bool {
        let serviceManagementMatch = launchctlOutput.contains(
            "parent bundle identifier = \(bundleIdentifier)"
        ) && launchctlOutput.contains("parent bundle version = \(bundleVersion)")
        return serviceManagementMatch
            || launchctlOutput.contains(bundlePath)
    }

    static func connectorPIDs(psOutput: String) -> [Int32] {
        psOutput.split(separator: "\n").compactMap { row in
            let fields = row.split(whereSeparator: \.isWhitespace)
            guard fields.count >= 3,
                  let pid = Int32(fields[0]),
                  pid != getpid(),
                  URL(fileURLWithPath: String(fields[1])).lastPathComponent == "plug",
                  fields.dropFirst(2).contains("connect")
            else { return nil }
            return pid
        }
    }
}
