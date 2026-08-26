import Foundation

public struct OperatorHandshake: Codable, Equatable, Sendable {
    public let daemonVersion: String
    public let daemonExecutable: URL?
    public let ipcMin: UInt16
    public let ipcMax: UInt16
    public let ownership: String
    public let capabilities: [String]

    private enum CodingKeys: String, CodingKey {
        case daemonVersion, daemonExecutable, ipcMin, ipcMax, ownership, capabilities
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        daemonVersion = try container.decode(String.self, forKey: .daemonVersion)
        if let value = try container.decodeIfPresent(String.self, forKey: .daemonExecutable) {
            daemonExecutable = value.contains("://")
                ? URL(string: value)
                : URL(fileURLWithPath: value)
        } else {
            daemonExecutable = nil
        }
        ipcMin = try container.decode(UInt16.self, forKey: .ipcMin)
        ipcMax = try container.decode(UInt16.self, forKey: .ipcMax)
        ownership = try container.decode(String.self, forKey: .ownership)
        capabilities = try container.decode([String].self, forKey: .capabilities)
    }
}

public struct ServerStatus: Codable, Identifiable, Equatable, Sendable {
    public var id: String { serverId }
    public let serverId: String
    public let health: String
    public let toolCount: Int
    public let error: String?
}

public struct ConfiguredServer: Codable, Identifiable, Equatable, Sendable {
    public var id: String { name }
    public let name: String
    public let enabled: Bool
    public let transport: String
    public let oauth: Bool
}

public struct LiveSession: Codable, Identifiable, Equatable, Sendable {
    public var id: String { sessionId }
    public let transport: String
    public let clientId: String?
    public let sessionId: String
    public let clientType: String
    public let clientInfo: String?
    public let connectedSecs: UInt64
    public let lastActivitySecs: UInt64?
}

public struct ClientVisibility: Codable, Equatable, Sendable {
    public let sessionId: String
    public let clientType: String
    public let visibleToolCount: Int
}

public struct AuthServer: Codable, Identifiable, Equatable, Sendable {
    public var id: String { name }
    public let name: String
    public let url: String?
    public let authenticated: Bool
    public let health: String
    public let scopes: [String]?
    public let tokenExpiresInSecs: UInt64?
    public let warnings: [String]
}

public struct DownstreamClient: Codable, Identifiable, Equatable, Sendable {
    public var id: String { clientId }
    public let clientId: String
    public let clientName: String
    public let redirectUris: [String]
    public let source: String
}

public struct OperatorSnapshot: Codable, Equatable, Sendable {
    public let runtimeVersion: String
    public let uptimeSecs: UInt64
    public let ownership: String
    public let configuredServers: [ConfiguredServer]
    public let servers: [ServerStatus]
    public let liveSessions: [LiveSession]
    public let clientVisibility: [ClientVisibility]
    public let upstreamAuth: [AuthServer]
    public let downstreamClients: [DownstreamClient]

    public static let empty = OperatorSnapshot(
        runtimeVersion: "", uptimeSecs: 0, ownership: "unmanaged", configuredServers: [], servers: [],
        liveSessions: [], clientVisibility: [], upstreamAuth: [], downstreamClients: []
    )
}

public struct ActivityEvent: Codable, Identifiable, Equatable, Sendable {
    public var id: UInt64 { sequence }
    public let sequence: UInt64
    public let occurredAtMs: UInt64
    /// Per-connection identity. For a local editor this is a per-process UUID,
    /// so it separates one window from another but means nothing to a reader.
    public let client: String?
    public let method: String
    public let server: String?
    public let tool: String?
    public let clientType: String?
    public let clientLabel: String?
    public let latencyMs: UInt64
    public let outcome: String

    public init(
        sequence: UInt64,
        occurredAtMs: UInt64,
        client: String?,
        method: String,
        server: String?,
        tool: String? = nil,
        clientType: String? = nil,
        clientLabel: String? = nil,
        latencyMs: UInt64,
        outcome: String
    ) {
        self.sequence = sequence
        self.occurredAtMs = occurredAtMs
        self.client = client
        self.method = method
        self.server = server
        self.tool = tool
        self.clientType = clientType
        self.clientLabel = clientLabel
        self.latencyMs = latencyMs
        self.outcome = outcome
    }
}

public struct ToolInfo: Codable, Identifiable, Equatable, Sendable {
    public var id: String { name }
    /// Merged name downstream clients call, already server-prefixed.
    public let name: String
    public let serverId: String
    public let description: String?
    public let title: String?
    /// Hidden from downstream clients by a `disabled_tools` entry.
    public let disabled: Bool
    /// Set when a wildcard, rather than this tool's own name, is what hides it.
    public let disabledByPattern: String?

    public init(
        name: String,
        serverId: String,
        description: String? = nil,
        title: String? = nil,
        disabled: Bool = false,
        disabledByPattern: String? = nil
    ) {
        self.name = name
        self.serverId = serverId
        self.description = description
        self.title = title
        self.disabled = disabled
        self.disabledByPattern = disabledByPattern
    }

    private enum CodingKeys: String, CodingKey {
        case name, serverId, description, title, disabled, disabledByPattern
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        name = try container.decode(String.self, forKey: .name)
        serverId = try container.decode(String.self, forKey: .serverId)
        description = try container.decodeIfPresent(String.self, forKey: .description)
        title = try container.decodeIfPresent(String.self, forKey: .title)
        disabled = try container.decodeIfPresent(Bool.self, forKey: .disabled) ?? false
        disabledByPattern = try container.decodeIfPresent(String.self, forKey: .disabledByPattern)
    }
}

public struct ServerConfig: Codable, Equatable, Sendable {
    public var command: String?
    public var args: [String] = []
    public var env: [String: String] = [:]
    public var enabled = true
    public var transport: String
    public var protocolMode = "legacy"
    public var url: String?
    public var authToken: String?
    public var auth: String?
    public var oauthClientID: String?
    public var oauthScopes: [String]?
    public var timeoutSecs = 30
    public var callTimeoutSecs = 300
    public var maxConcurrent = 1
    public var healthCheckIntervalSecs = 60
    public var circuitBreakerEnabled = true
    public var enrichment = false
    public var toolRenames: [String: String] = [:]
    public var toolGroups: [String] = []
    public var sandbox: String?

    public static func command(_ command: String, args: [String]) -> Self {
        Self(command: command, args: args, transport: "stdio")
    }

    public static func remote(_ url: String) -> Self {
        Self(transport: "http", url: url)
    }
}

public enum IPCRequest: Encodable, Equatable, Sendable {
    case handshake(clientVersion: String, ipcMin: UInt16, ipcMax: UInt16)
    case snapshot(authToken: String)
    case activity(authToken: String, afterSequence: UInt64, limit: Int, failuresOnly: Bool)
    case validateServer(authToken: String, name: String, server: ServerConfig)
    case addServer(authToken: String, name: String, server: ServerConfig)
    case updateServer(authToken: String, name: String, server: ServerConfig)
    case removeServer(authToken: String, name: String)
    case setServerEnabled(authToken: String, name: String, enabled: Bool)
    case listTools
    case setToolEnabled(authToken: String, tool: String, enabled: Bool)
    case restartServer(authToken: String, serverID: String)
    case revokeClient(authToken: String, clientID: String)
    case shutdown(authToken: String)

    private enum CodingKeys: String, CodingKey {
        case type, clientVersion, ipcMin, ipcMax, authToken, afterSequence, limit, failuresOnly
        case name, server, enabled, serverID, clientID, tool
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .handshake(version, min, max):
            try c.encode("OperatorHandshake", forKey: .type); try c.encode(version, forKey: .clientVersion)
            try c.encode(min, forKey: .ipcMin); try c.encode(max, forKey: .ipcMax)
        case let .snapshot(token):
            try c.encode("OperatorSnapshot", forKey: .type); try c.encode(token, forKey: .authToken)
        case let .activity(token, after, limit, failures):
            try c.encode("ActivitySnapshot", forKey: .type); try c.encode(token, forKey: .authToken)
            try c.encode(after, forKey: .afterSequence); try c.encode(limit, forKey: .limit)
            try c.encode(failures, forKey: .failuresOnly)
        case let .validateServer(token, name, server):
            try c.encode("ValidateServer", forKey: .type); try c.encode(token, forKey: .authToken)
            try c.encode(name, forKey: .name); try c.encode(server, forKey: .server)
        case let .addServer(token, name, server):
            try c.encode("AddServer", forKey: .type); try c.encode(token, forKey: .authToken)
            try c.encode(name, forKey: .name); try c.encode(server, forKey: .server)
        case let .updateServer(token, name, server):
            try c.encode("UpdateServer", forKey: .type); try c.encode(token, forKey: .authToken)
            try c.encode(name, forKey: .name); try c.encode(server, forKey: .server)
        case let .removeServer(token, name):
            try c.encode("RemoveServer", forKey: .type); try c.encode(token, forKey: .authToken); try c.encode(name, forKey: .name)
        case let .setServerEnabled(token, name, enabled):
            try c.encode("SetServerEnabled", forKey: .type); try c.encode(token, forKey: .authToken)
            try c.encode(name, forKey: .name); try c.encode(enabled, forKey: .enabled)
        case .listTools:
            try c.encode("ListTools", forKey: .type)
        case let .setToolEnabled(token, tool, enabled):
            try c.encode("SetToolEnabled", forKey: .type); try c.encode(token, forKey: .authToken)
            try c.encode(tool, forKey: .tool); try c.encode(enabled, forKey: .enabled)
        case let .restartServer(token, serverID):
            try c.encode("RestartServer", forKey: .type); try c.encode(token, forKey: .authToken)
            try c.encode(serverID, forKey: .serverID)
        case let .revokeClient(token, clientID):
            try c.encode("RevokeDownstreamClient", forKey: .type); try c.encode(token, forKey: .authToken)
            try c.encode(clientID, forKey: .clientID)
        case let .shutdown(token):
            try c.encode("Shutdown", forKey: .type); try c.encode(token, forKey: .authToken)
        }
    }
}

public enum IPCResponse: Decodable, Sendable {
    case handshake(OperatorHandshake)
    case snapshot(OperatorSnapshot)
    case activity([ActivityEvent])
    case tools([ToolInfo])
    case validated
    case mutation
    case revoked(String)
    case ok
    case error(code: String, message: String)

    private enum CodingKeys: String, CodingKey {
        case type, handshake, snapshot, events, tools, clientId, code, message
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)
        switch type {
        case "OperatorHandshake": self = .handshake(try c.decode(OperatorHandshake.self, forKey: .handshake))
        case "OperatorSnapshot": self = .snapshot(try c.decode(OperatorSnapshot.self, forKey: .snapshot))
        case "ActivitySnapshot": self = .activity(try c.decode([ActivityEvent].self, forKey: .events))
        case "Tools": self = .tools(try c.decode([ToolInfo].self, forKey: .tools))
        case "ServerValidated": self = .validated
        case "OperatorMutation": self = .mutation
        case "DownstreamClientRevoked": self = .revoked(try c.decode(String.self, forKey: .clientId))
        case "Ok": self = .ok
        case "Error": self = .error(code: try c.decode(String.self, forKey: .code), message: try c.decode(String.self, forKey: .message))
        default: throw PlugIPCError.unexpectedResponse(type)
        }
    }
}
