import Foundation
import PlugIPC

/// A server's config and what it said about itself, joined by name.
struct ServerOrigin: Equatable, Sendable {
    var config: ServerConfig?
    var upstream: UpstreamInfo?
}

/// How the app asks the daemon anything. Every request but the handshake
/// carries the operator token, read from its file per request so a token the
/// daemon rotated is picked up without a restart.
@MainActor
struct DaemonClient {
    let clientVersion: String
    private let ipc: PlugIPCClient
    private let tokenURL: URL

    init(ipc: PlugIPCClient, clientVersion: String, tokenURL: URL) {
        self.ipc = ipc
        self.clientVersion = clientVersion
        self.tokenURL = tokenURL
    }

    func handshake() async throws -> OperatorHandshake { try await ipc.connect() }

    func disconnect() async { await ipc.disconnect() }

    /// Sends one request built around the token.
    func send(_ request: (String) -> IPCRequest) async throws -> IPCResponse {
        try await ipc.request(request(token()))
    }

    func snapshot() async throws -> OperatorSnapshot {
        guard case let .snapshot(value) = try await send({ .snapshot(authToken: $0) }) else {
            throw PlugIPCError.unexpectedResponse("OperatorSnapshot")
        }
        return value
    }

    /// Nil when the daemon answered with something other than calls.
    func activity(after cursor: UInt64, limit: Int) async throws -> [ActivityEvent]? {
        guard case let .activity(events) = try await send({
            .activity(authToken: $0, afterSequence: cursor, limit: limit, failuresOnly: false)
        }) else { return nil }
        return events
    }

    /// Nil when the daemon answered with something other than tools.
    func tools() async throws -> [ToolInfo]? {
        guard case let .tools(tools) = try await ipc.request(.listTools) else { return nil }
        return tools
    }

    func serverConfig(name: String) async throws -> ServerConfig {
        guard case let .serverConfig(returnedName, config) = try await send({
            .serverConfig(authToken: $0, name: name)
        }), returnedName == name else {
            throw PlugIPCError.unexpectedResponse("ServerConfig")
        }
        return config
    }

    /// What every server said about itself, with the configs of the servers
    /// named. There is no request for every config at once, so each named
    /// server costs one request; a failed read leaves that part out.
    func origins(configsFor names: [String]) async -> [String: ServerOrigin] {
        var origins: [String: ServerOrigin] = [:]
        if case let .status(servers)? = try? await ipc.request(.status) {
            for server in servers where origins[server.serverId] == nil {
                origins[server.serverId] = ServerOrigin(upstream: server.upstream)
            }
        }
        for name in names {
            origins[name, default: ServerOrigin()].config = try? await serverConfig(name: name)
        }
        return origins
    }

    /// The operations of the OpenAPI document at `spec`. The daemon may have
    /// to fetch the document, which outlasts the poll's short timeout, so
    /// this asks over a connection of its own.
    func describeAPI(_ spec: String) async throws -> APISummary {
        let token = try token()
        let patient = PlugIPCClient(clientVersion: clientVersion, requestTimeout: 15)
        guard case let .apiDescribed(api) = try await patient.request(
            .describeAPI(authToken: token, spec: spec)
        ) else {
            throw PlugIPCError.unexpectedResponse("ApiDescribed")
        }
        return api
    }

    private func token() throws -> String {
        try String(contentsOf: tokenURL, encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
