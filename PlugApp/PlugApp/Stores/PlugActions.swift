import Foundation
import Observation
import PlugIPC

/// The changes a person asks Plug for, and what the interface shows while
/// they run: which presses are busy, which sign-ins are open, and the press
/// that failed. Each change is followed by one read of the daemon, so a sheet
/// that sends two requests, or one per server, still costs one read.
@MainActor @Observable
final class PlugActions {
    private let daemon: DaemonClient
    private let appLinker: any AppLinking
    private let authFlow: AuthFlowService
    private let canMutate: @MainActor () -> Bool
    private let refresh: @MainActor () async -> Void

    /// A button press that failed. Kept apart from the connection's error
    /// because a poll that succeeds says nothing about the press, and
    /// clearing one with the other used to wipe the message before anyone
    /// could read it.
    private(set) var actionError: ActionError?
    private(set) var signingInServers: Set<String> = []
    /// The running `plug auth login` per server, so Cancel and Try Again can
    /// stop it instead of leaving it waiting on the browser.
    private var signInTasks: [String: Task<Void, Never>] = [:]
    private(set) var busyTools: Set<String> = []
    private(set) var busyApps: Set<String> = []
    private(set) var connectableApps: [LinkableApp] = []
    private(set) var hasLoadedConnectableApps = false
    private(set) var isLoadingConnectableApps = false
    private(set) var connectableAppsError: String?

    init(
        daemon: DaemonClient,
        appLinker: any AppLinking,
        authFlow: AuthFlowService,
        canMutate: @escaping @MainActor () -> Bool,
        refresh: @escaping @MainActor () async -> Void
    ) {
        self.daemon = daemon
        self.appLinker = appLinker
        self.authFlow = authFlow
        self.canMutate = canMutate
        self.refresh = refresh
    }

    // MARK: Daemon changes

    /// Runs one change. `doing` finishes the sentence "Could not …", so a
    /// failure names what was being attempted.
    func perform(_ doing: String, _ request: (String) -> IPCRequest) async {
        do { try await change { try await $0.send(request) } } catch { report(doing, error) }
    }

    /// Checks a new server with the daemon before adding it, so a server that
    /// cannot start is refused with the daemon's reason.
    func addServer(_ name: String, _ config: ServerConfig, secretStore: String?) async throws {
        try await change { daemon in
            _ = try await daemon.send { .validateServer(authToken: $0, name: name, server: config) }
            _ = try await daemon.send {
                .addServer(authToken: $0, name: name, server: config, secretStore: secretStore)
            }
        }
    }

    func updateServer(_ name: String, _ config: ServerConfig, secretStore: String?) async throws {
        try await change { daemon in
            _ = try await daemon.send { .validateServer(authToken: $0, name: name, server: config) }
            _ = try await daemon.send {
                .updateServer(authToken: $0, name: name, server: config, secretStore: secretStore)
            }
        }
    }

    /// Adds each server in turn. Returns why each one that failed did, by
    /// its id; the rest went in.
    func importServers(_ servers: [DiscoveredServer]) async throws -> [String: String] {
        var failed: [String: String] = [:]
        try await change { daemon in
            for server in servers {
                do {
                    _ = try await daemon.send { .addServer(authToken: $0, name: server.name, server: server.config) }
                } catch {
                    failed[server.id] = error.localizedDescription
                }
            }
        }
        return failed
    }

    func addAccount(server: String, label: String) async throws {
        try await change { _ = try await $0.send { .addAccount(authToken: $0, server: server, account: label) } }
    }

    func addWatch(_ watch: WatchConfig) async throws {
        try await change { _ = try await $0.send { .addWatch(authToken: $0, watch: watch) } }
    }

    func nameClient(key: String, name: String) async throws {
        try await change { _ = try await $0.send { .renameClient(authToken: $0, key: key, name: name) } }
    }

    /// The settings move first: if that fails, the old sign-in keeps both its
    /// settings and its access.
    func replaceClient(oldID: String, newID: String) async {
        do {
            try await change { daemon in
                _ = try await daemon.send {
                    .moveClientSettings(authToken: $0, from: "oauth:\(oldID)", to: "oauth:\(newID)")
                }
                _ = try await daemon.send { .revokeClient(authToken: $0, clientID: oldID) }
            }
        } catch { report("replace the old sign-in", error) }
    }

    func setToolEnabled(_ tool: String, _ enabled: Bool) async {
        guard busyTools.insert(tool).inserted else { return }
        defer { busyTools.remove(tool) }
        await perform("turn \(tool) \(enabled ? "on" : "off")") {
            .setToolEnabled(authToken: $0, tool: tool, enabled: enabled)
        }
    }

    /// The operations of the OpenAPI document at `spec`.
    func describeAPI(_ spec: String) async throws -> APISummary {
        guard canMutate() else { throw RuntimeUnavailableError() }
        return try await daemon.describeAPI(spec)
    }

    /// No forced tool list afterwards. Every change a request can make to it
    /// moves the snapshot's `tool_catalog_revision`, and the read refetches
    /// on that alone.
    private func change(_ requests: (DaemonClient) async throws -> Void) async throws {
        guard canMutate() else { throw RuntimeUnavailableError() }
        try await requests(daemon)
        await refresh()
    }

    // MARK: Sign-in

    /// Starts a sign-in. Pressed again while one is open, it is Try Again:
    /// the open attempt holds the browser callback, so it is stopped and
    /// waited for before the new one starts.
    func signIn(server: String) async {
        if let running = signInTasks[server] {
            running.cancel()
            await running.value
            // A second press that waited on the same attempt has already
            // started the replacement; one browser tab is enough.
            if let current = signInTasks[server], current != running { return }
        }
        let task = Task { [weak self] in _ = await self?.runSignIn(server: server) }
        signInTasks[server] = task
        await task.value
        if signInTasks[server] == task { signInTasks[server] = nil }
    }

    /// Stops an open sign-in. The browser tab stays, but nothing is waiting
    /// on it any more, and the server goes back to offering Sign In.
    func cancelSignIn(server: String) {
        signInTasks[server]?.cancel()
    }

    /// The button that starts this is behind a confirmation, so by the time
    /// it runs the choice has been made.
    func signOut(server: String) async {
        do {
            try await authFlow.signOut(server: server)
            await refresh()
        } catch { report("sign out of \(server)", error) }
    }

    /// Runs a client's sign-in command. The client opens the browser, where
    /// Plug asks the owner to approve it.
    func signInClient(name: String, command: String) async {
        do {
            try await authFlow.signInClient(command: command)
            await refresh()
        } catch {
            report("sign in \(name)", error)
        }
    }

    private func runSignIn(server: String) async {
        signingInServers.insert(server)
        defer { signingInServers.remove(server) }
        do {
            try await authFlow.signIn(server: server)
            await refresh()
        } catch let error where Task.isCancelled || error is CancellationError {
            // Cancelled on purpose. Nothing failed.
        } catch {
            report("sign in to \(server)", error)
        }
    }

    // MARK: Clients on this Mac

    /// Which AI apps are wired into Plug. Read from the client configuration
    /// files on disk rather than the daemon, which does not own them.
    func loadConnectableApps() async {
        guard !isLoadingConnectableApps else { return }
        isLoadingConnectableApps = true
        defer {
            isLoadingConnectableApps = false
            if !hasLoadedConnectableApps { hasLoadedConnectableApps = true }
        }
        do {
            // Assigned only when it differs: the list is read every few
            // seconds while shown, and an equal value should redraw nothing.
            let apps = try await appLinker.apps()
            if apps != connectableApps { connectableApps = apps }
            if connectableAppsError != nil { connectableAppsError = nil }
        } catch {
            let message = error.localizedDescription
            if connectableAppsError != message { connectableAppsError = message }
        }
    }

    func setAppLinked(_ target: String, _ linked: Bool, overNetwork: Bool = false) async {
        guard busyApps.insert(target).inserted else { return }
        defer { busyApps.remove(target) }
        do {
            if linked {
                try await appLinker.link(target: target, overNetwork: overNetwork)
            } else {
                try await appLinker.unlink(target: target)
            }
            await loadConnectableApps()
            await refresh()
        } catch {
            let name = AppIcons.displayName(forTarget: target) ?? target
            report(linked ? "add Plug to \(name)" : "remove Plug from \(name)", error)
        }
    }

    // MARK: Failures

    /// Shows a failed press until it is dismissed. A newer failure replaces
    /// it. It does not leave by itself: a message that vanishes while it is
    /// being read is no message.
    func report(_ doing: String, _ error: any Error) {
        actionError = ActionError(
            title: "Could not \(doing).",
            message: error.localizedDescription,
            advice: Explain.advice(for: error)
        )
    }

    func dismissActionError() {
        actionError = nil
    }
}

/// A press that failed: what was being done, why it failed in the error's
/// own words, and what to do next.
struct ActionError: Identifiable, Equatable {
    let id = UUID()
    let title: String
    let message: String
    let advice: String
}

private struct RuntimeUnavailableError: LocalizedError {
    var errorDescription: String? {
        "Plug is starting. Try again in a moment."
    }
}
