import Foundation
import Observation
import PlugIPC

@MainActor @Observable
final class AppModel {
    enum ConnectionState: Equatable { case disconnected, connecting, incompatible, ready }

    private let ipc: PlugIPCClient
    private var monitoringTask: Task<Void, Never>?
    private var refreshTask: Task<Void, Never>?
    private(set) var connectionState: ConnectionState = .disconnected
    private(set) var snapshot: OperatorSnapshot = .empty
    private(set) var activities: [ActivityEvent] = []
    private(set) var lastError: String?
    private(set) var serviceNeedsAdoption = DaemonServiceManager.shared.needsAdoption

    init(ipc: PlugIPCClient = PlugIPCClient()) { self.ipc = ipc }

    var visibleServers: [ServerStatus] {
        snapshot.servers.enumerated().sorted {
            let lhsBad = $0.element.health != "Healthy"
            let rhsBad = $1.element.health != "Healthy"
            return lhsBad == rhsBad ? $0.offset < $1.offset : lhsBad && !rhsBad
        }.map(\.element)
    }

    var menuBarSymbol: String {
        if connectionState != .ready { return "bolt.slash.circle" }
        return snapshot.servers.contains { $0.health == "Failed" || $0.health == "AuthRequired" }
            ? "bolt.trianglebadge.exclamationmark" : "bolt.circle.fill"
    }

    var isHealthy: Bool { connectionState == .ready && visibleServers.allSatisfy { $0.health == "Healthy" } }

    func startMonitoring() async {
        guard monitoringTask == nil else { return }
        monitoringTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }

    func refresh() async {
        guard refreshTask == nil else { return }
        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            connectionState = .connecting
            do {
                let handshake = try await ipc.connect()
                guard handshake.ipcMin <= 4, handshake.ipcMax >= 3 else {
                    connectionState = .incompatible; return
                }
                let token = try String(contentsOf: PlugIPCClient.defaultTokenURL, encoding: .utf8)
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                guard case let .snapshot(value) = try await ipc.request(.snapshot(authToken: token)) else {
                    throw PlugIPCError.unexpectedResponse("OperatorSnapshot")
                }
                snapshot = value
                if case let .activity(events) = try await ipc.request(
                    .activity(authToken: token, afterSequence: 0, limit: 200, failuresOnly: false)
                ) { activities = events }
                connectionState = .ready
                lastError = nil
            } catch {
                connectionState = .disconnected
                lastError = error.localizedDescription
            }
        }
        refreshTask = task
        await task.value
        refreshTask = nil
    }

    func perform(_ request: (String) -> IPCRequest) async {
        do {
            let token = try String(contentsOf: PlugIPCClient.defaultTokenURL, encoding: .utf8)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            _ = try await ipc.request(request(token))
            await refresh()
        } catch { lastError = error.localizedDescription }
    }

    func adoptDaemon() async {
        do {
            try DaemonServiceManager.shared.adopt()
            serviceNeedsAdoption = false
            try await Task.sleep(for: .milliseconds(700))
            await refresh()
        } catch { lastError = error.localizedDescription }
    }

    func restartDaemon() async {
        do {
            try await DaemonServiceManager.shared.restart()
            try await Task.sleep(for: .milliseconds(700))
            await refresh()
        } catch { lastError = error.localizedDescription }
    }
}
