import XCTest
@testable import PlugIPC

/// The app against the wire the daemon speaks, as `testdata/ipc` pins it.
/// `plug-core/tests/ipc_goldens.rs` checks the same files against the
/// daemon's types and fails when they drift, so a change on either side that
/// the other does not make fails one of the two.
final class GoldenPayloadTests: XCTestCase {
    private let decoder = PlugIPCClient.makeDecoder()

    private static let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()

    private func golden(_ name: String) throws -> Data {
        try Data(contentsOf: Self.repoRoot.appending(path: "testdata/ipc/\(name)"))
    }

    /// Each golden in a file of them, as the JSON the daemon wrote.
    private func goldens(_ name: String) throws -> [String: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: golden(name)) as? [String: Any])
    }

    // MARK: Requests

    private static let token = "golden-auth-token"

    private static let stdioServer = ServerConfig(
        command: "uvx",
        args: ["workspace-mcp", "--single-user"],
        env: ["WORKSPACE_TOKEN": "$WORKSPACE_TOKEN"],
        enabled: false,
        transport: "stdio",
        timeoutSecs: 45,
        callTimeoutSecs: 600,
        maxConcurrent: 2,
        healthCheckIntervalSecs: 120,
        circuitBreakerEnabled: false,
        toolRenames: ["search": "find"],
        toolGroups: [ToolGroupRule(prefix: "Gmail", contains: ["gmail"], strip: ["gmail"])],
        sandbox: StdioSandboxConfig(enabled: true, allowNetwork: true, allowRead: ["/Users/me/notes"])
    )

    private static let remoteServer = ServerConfig(
        transport: "sse",
        url: "https://example.com/sse",
        auth: "oauth",
        oauthClientID: "plug",
        oauthScopes: ["read"]
    )

    private static let apiServer = ServerConfig(
        transport: "openapi",
        authToken: "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}",
        maxConcurrent: 4,
        spec: "https://example.com/openapi.json",
        operations: ["listPets"],
        tokenIn: "header:X-Api-Key"
    )

    /// Every request the app sends, by its name in `requests.json`.
    private static let requests: [String: IPCRequest] = {
        func op(_ request: OperatorRequest) -> IPCRequest { .operator(token: token, request) }
        func mutate(_ mutation: OperatorMutation) -> IPCRequest { op(.mutate(mutation)) }
        func block(_ kind: ClientBlockKind, _ target: String, _ blocked: Bool) -> IPCRequest {
            mutate(.setClientBlock(key: "claude-code", kind: kind, target: target, blocked: blocked))
        }
        return [
            "handshake": .handshake(clientVersion: "0.9.0"),
            "status": .status,
            "list_tools": .listTools,
            "snapshot": op(.snapshot),
            "activity": op(.activity(afterSequence: 41, limit: 200, failuresOnly: false)),
            "get_server_config": op(.serverConfig(name: "workspace")),
            "validate_server": op(.validateServer(name: "workspace", server: stdioServer)),
            "describe_api": op(.describeAPI(spec: "https://example.com/openapi.json")),
            "add_server": op(.mutate(.addServer(name: "workspace", server: stdioServer), secretStore: "file")),
            "add_remote_server": mutate(.addServer(name: "notes", server: remoteServer)),
            "update_server": mutate(.updateServer(name: "pets", server: apiServer)),
            "import_servers": mutate(.importServers([ImportedServer(
                name: "github",
                sourceTarget: "cursor",
                server: .command("npx", args: ["-y", "github-mcp"])
            )])),
            "remove_server": mutate(.removeServer(name: "workspace")),
            "add_account": mutate(.addAccount(server: "workspace", account: "work")),
            "set_server_enabled": mutate(.setServerEnabled(name: "workspace", enabled: false)),
            "set_tool_enabled": mutate(.setToolEnabled(tool: "Gmail__send", enabled: true)),
            "rename_client": mutate(.renameClient(key: "oauth:abc", name: "Work laptop")),
            "move_client_settings": mutate(.moveClientSettings(from: "oauth:old", to: "oauth:new")),
            "set_client_place": mutate(.setClientPlace(key: "oauth:abc", place: "Studio Mac")),
            "block_server": block(.server, "slack", true),
            "block_tool": block(.tool, "Gmail__send", false),
            "allow_server": block(.allowedServer, "github", true),
            "allow_tool": block(.allowedTool, "GitHub__search", false),
            "allow_list": block(.allowList, "*", true),
            "add_watch": mutate(.addWatch(WatchConfig(
                name: "inbox",
                server: "workspace",
                tool: "search_gmail_messages",
                arguments: ["query": .string("is:unread"), "maxResults": .number(5)],
                everySecs: 60,
                allowWrites: true
            ))),
            "remove_watch": mutate(.removeWatch(event: "workspace.inbox")),
            "restart_server": op(.restartServer(serverID: "workspace")),
            "reload": op(.reload),
            "revoke_client": op(.revokeClient(clientID: "abc")),
            "shutdown": op(.shutdown),
        ]
    }()

    func testEveryRequestGoldenHasAnAppRequestAndNoneIsLeftOut() throws {
        XCTAssertEqual(Set(try goldens("requests.json").keys), Set(Self.requests.keys))
    }

    /// The app writes each request as the daemon does. The daemon writes a
    /// field with nothing in it as `null` and the app leaves it out, which
    /// the daemon reads the same way. A server setting the app leaves out is
    /// one the daemon fills with its default, from `server_config_default.json`.
    func testTheAppWritesEveryRequestAsTheDaemonDoes() throws {
        let golden = try goldens("requests.json")
        let defaults = try goldens("server_config_default.json")
        let encoder = PlugIPCClient.makeEncoder()
        for (name, request) in Self.requests {
            let sent = Self.withServerDefaults(
                try JSONSerialization.jsonObject(with: encoder.encode(request)), defaults
            )
            XCTAssertEqual(
                Self.withoutNulls(sent) as? NSDictionary,
                Self.withoutNulls(golden[name] as Any) as? NSDictionary,
                name
            )
        }
    }

    /// `value` with each server in it filled in where the daemon would fill
    /// it, for a field the app left out.
    private static func withServerDefaults(_ value: Any, _ defaults: [String: Any]) -> Any {
        if let object = value as? [String: Any] {
            var filled = object.mapValues { withServerDefaults($0, defaults) }
            if var server = filled["server"] as? [String: Any] {
                for (key, value) in defaults where server[key] == nil { server[key] = value }
                filled["server"] = server
            }
            return filled
        }
        if let array = value as? [Any] { return array.map { withServerDefaults($0, defaults) } }
        return value
    }

    private static func withoutNulls(_ value: Any) -> Any {
        switch value {
        case let object as [String: Any]:
            object.filter { !($0.value is NSNull) }.mapValues(withoutNulls)
        case let array as [Any]:
            array.map(withoutNulls)
        default:
            value
        }
    }

    // MARK: Responses

    private func response(_ name: String) throws -> IPCResponse {
        let json = try XCTUnwrap(goldens("responses.json")[name], "no response golden \(name)")
        return try decoder.decode(IPCResponse.self, from: JSONSerialization.data(withJSONObject: json))
    }

    func testEveryResponseGoldenDecodes() throws {
        for name in try goldens("responses.json").keys {
            XCTAssertNoThrow(try response(name), name)
        }
    }

    func testHandshake() throws {
        guard case let .handshake(handshake) = try response("handshake") else { return XCTFail() }
        XCTAssertEqual(handshake.daemonVersion, "0.9.0")
        XCTAssertEqual(handshake.daemonExecutable?.path, "/Applications/Plug.app/Contents/Resources/plug")
        XCTAssertEqual(handshake.ownership, .appManaged)
    }

    func testSnapshot() throws {
        guard case let .snapshot(snapshot) = try response("snapshot") else { return XCTFail() }
        XCTAssertEqual(snapshot.runtimeVersion, "0.9.0")
        XCTAssertEqual(snapshot.prefixDelimiter, ".")
        XCTAssertEqual(snapshot.toolCatalogRevision, 7)
        XCTAssertEqual(snapshot.ownership, .appManaged)
        XCTAssertEqual(snapshot.configuredServers.map(\.name), ["healthy", "degraded", "failed", "auth-required", "notes"])
        XCTAssertEqual(snapshot.configuredServers.map(\.transport), ["stdio", "stdio", "stdio", "stdio", "sse"])
        XCTAssertEqual(snapshot.configuredServers.map(\.oauth), [false, false, false, true, true])
        XCTAssertEqual(snapshot.servers.map(\.health), [.healthy, .degraded, .failed, .authRequired])
        XCTAssertEqual(snapshot.servers.map(\.toolCount), [0, 4, 8, 12])
        XCTAssertEqual(snapshot.servers[2].error, "failed to spawn `npx`: not found")
        XCTAssertEqual(snapshot.servers[0].upstream?.websiteUrl, "https://example.com")

        let local = snapshot.liveSessions[0]
        XCTAssertEqual(local.transport, "daemon_proxy")
        XCTAssertEqual(local.clientType, "ClaudeCode")
        XCTAssertEqual(local.host?.executable, "/opt/homebrew/bin/claude")
        XCTAssertEqual(local.clientKey, "claude-code")
        XCTAssertEqual(local.accessKey, "claude-code")
        XCTAssertEqual(local.target, "claude-code")
        let remote = snapshot.liveSessions[1]
        XCTAssertEqual(remote.transport, "http")
        XCTAssertEqual(remote.clientKey, "oauth:abc")
        XCTAssertNil(remote.target)

        XCTAssertEqual(snapshot.upstreamAuth.first?.tokenExpiresInSecs, 7_300)
        XCTAssertEqual(snapshot.upstreamAuth.first?.warnings, ["token expires soon"])
        let grant = try XCTUnwrap(snapshot.downstreamClients.first)
        XCTAssertEqual(grant.clientName, "ChatGPT")
        XCTAssertEqual(grant.signedIn, true)
        XCTAssertEqual(grant.lastUsedAt, 1_700_000_500)
        XCTAssertEqual(grant.clientKey, "oauth:abc")
        XCTAssertNil(grant.target)
        XCTAssertEqual(snapshot.clientNames, [ClientName(key: "oauth:abc", name: "Work laptop")])
        XCTAssertEqual(snapshot.clientPlaces, [ClientPlace(key: "oauth:abc", place: "Studio Mac")])

        let blocks = try XCTUnwrap(snapshot.clientBlocks)
        XCTAssertEqual(blocks[0].servers, ["degraded"])
        XCTAssertEqual(blocks[0].tools, ["healthy__send", "failed__*"])
        XCTAssertEqual(blocks[1].onlyAllowed, true)
        XCTAssertEqual(blocks[1].allowedServers, ["healthy"])
        XCTAssertEqual(blocks[1].allowedTools, ["failed__read"])
        XCTAssertEqual(blocks[0].offServers, ["degraded"])
        XCTAssertEqual(blocks[0].offTools, [
            OffTool(name: "healthy__send", rule: nil), OffTool(name: "failed__read", rule: "failed__*"),
        ])
        XCTAssertEqual(blocks[1].offServers, ["degraded", "notes"])
        XCTAssertEqual(blocks[1].offTools?.map(\.name), ["failed__write"])

        let events = try XCTUnwrap(snapshot.events)
        XCTAssertEqual(
            events.map(\.state),
            [.waiting, .watching, .toolMissing, .notReadOnly, .callFailed, .tooLarge, .watching]
        )
        XCTAssertEqual(events[0].lastChanged, 1_700_000_000)
        XCTAssertNil(events[1].lastChanged)
        XCTAssertEqual(events[5].subscribers, 5)
        XCTAssertNil(events[6].tool)
        XCTAssertNil(events[6].everySecs)
        XCTAssertEqual(snapshot.clientAddress, "https://plug.example.com/mcp")
    }

    func testStatus() throws {
        guard case let .status(servers) = try response("status") else { return XCTFail() }
        XCTAssertEqual(servers.map(\.serverId), ["healthy"])
    }

    func testServerConfigs() throws {
        guard case let .serverConfig(name, server) = try response("server_config") else { return XCTFail() }
        XCTAssertEqual(name, "workspace")
        XCTAssertEqual(server, try asTheDaemonKeepsIt(Self.stdioServer))
        guard case let .serverConfig(_, api) = try response("api_server_config") else { return XCTFail() }
        XCTAssertEqual(api, try asTheDaemonKeepsIt(Self.apiServer))
        // A field whose name ends in an initialism is read back, so a save
        // does not drop it.
        guard case let .serverConfig(_, remote) = try response("remote_server_config") else { return XCTFail() }
        XCTAssertEqual(remote, try asTheDaemonKeepsIt(Self.remoteServer))
    }

    /// Every setting the daemon's `ServerConfig` has is a field here. A
    /// setting added to the daemon fails this until the app models it.
    func testTheAppModelsEveryServerSetting() throws {
        let server = try decoder.decode(ServerConfig.self, from: golden("server_config_every_field.json"))
        XCTAssertEqual(server.unmodeled, [:], "the app's ServerConfig has no field for these")
        let sent = try JSONSerialization.jsonObject(with: PlugIPCClient.makeEncoder().encode(server))
        XCTAssertEqual(
            sent as? NSDictionary,
            try JSONSerialization.jsonObject(with: golden("server_config_every_field.json")) as? NSDictionary
        )
    }

    /// A setting the app has no field for goes back to the daemon as it
    /// came, so editing a server in an older app keeps it.
    func testASettingTheAppDoesNotModelSurvivesASave() throws {
        var json = try goldens("server_config_default.json")
        json["future_setting"] = ["max_items": 3, "names": ["a"], "on": true]
        var server = try decoder.decode(ServerConfig.self, from: JSONSerialization.data(withJSONObject: json))
        server.args = ["--edited"]
        let sent = try XCTUnwrap(
            JSONSerialization.jsonObject(with: PlugIPCClient.makeEncoder().encode(server)) as? [String: Any]
        )
        XCTAssertEqual(
            sent["future_setting"] as? NSDictionary,
            ["max_items": 3, "names": ["a"], "on": true] as NSDictionary
        )
        XCTAssertEqual(sent["args"] as? [String], ["--edited"])
    }

    /// A server the app sent, read back once the daemon has filled in what
    /// the app left out.
    private func asTheDaemonKeepsIt(_ server: ServerConfig) throws -> ServerConfig {
        let sent = try JSONSerialization.jsonObject(with: PlugIPCClient.makeEncoder().encode(["server": server]))
        let kept = Self.withServerDefaults(sent, try goldens("server_config_default.json"))
        let data = try JSONSerialization.data(withJSONObject: kept)
        return try decoder.decode([String: ServerConfig].self, from: data)["server"]!
    }

    func testActivity() throws {
        guard case let .activity(events) = try response("activity") else { return XCTFail() }
        XCTAssertEqual(events.map(\.sequence), [42, 43, 44])
        XCTAssertEqual(events.map(\.outcome), [.success, .error, .cancelled])
        XCTAssertEqual(events[0].clientKey, "claude-code")
        XCTAssertEqual(events[0].clientLabel, "claude-code 2.0")
        XCTAssertEqual(events[0].tool, "healthy__send")
        XCTAssertEqual(events[0].target, "claude-code")
        XCTAssertEqual(events.map(\.remote), [false, true, true])
        XCTAssertEqual(events[1].reason, "upstream timed out")
        XCTAssertNil(events[2].server)
        XCTAssertEqual(events[2].clientKey, ActivityEvent.sharedRemoteKey)
    }

    func testTools() throws {
        guard case let .tools(tools) = try response("tools") else { return XCTFail() }
        XCTAssertEqual(tools.map(\.name), ["healthy__send", "failed__read"])
        XCTAssertEqual(tools[0].ownName, "send")
        XCTAssertEqual(tools[0].title, "Send")
        XCTAssertFalse(tools[0].readOnly)
        XCTAssertTrue(tools[1].disabled)
        XCTAssertEqual(tools[1].disabledByPattern, "failed__*")
        XCTAssertTrue(tools[1].readOnly)
    }

    func testApiDescribed() throws {
        guard case let .apiDescribed(api) = try response("api_described") else { return XCTFail() }
        XCTAssertEqual(api.title, "Pets")
        XCTAssertEqual(api.operations.map(\.name), ["listPets", "addPet"])
        XCTAssertEqual(api.operations[0].tag, "pets")
        XCTAssertNil(api.operations[1].tag)
    }

    func testSmallReplies() throws {
        guard case .validated = try response("server_validated") else { return XCTFail() }
        guard case let .mutation(none) = try response("mutation") else { return XCTFail() }
        XCTAssertEqual(none, [])
        // An import says the name each server went in under.
        guard case let .mutation(imported) = try response("imported") else { return XCTFail() }
        XCTAssertEqual(imported, ["github-cursor"])
        guard case let .revoked(id) = try response("revoked") else { return XCTFail() }
        XCTAssertEqual(id, "abc")
        guard case .reloaded = try response("reloaded") else { return XCTFail() }
        guard case .ok = try response("ok") else { return XCTFail() }
        guard case let .error(code, message) = try response("error") else { return XCTFail() }
        XCTAssertEqual(code, "OPERATOR_MUTATION_FAILED")
        XCTAssertEqual(message, "unknown server `gone`")
    }

    // MARK: Chunked

    func testAChunkedReplyReassemblesIntoTheReply() throws {
        let frames = try XCTUnwrap(JSONSerialization.jsonObject(with: golden("chunked_response.json")) as? [Any])
            .map { try JSONSerialization.data(withJSONObject: $0) }
        XCTAssertGreaterThan(frames.count, 2)
        var remaining = frames[...]
        let payload = try PlugIPCClient.readResponsePayload(decoder: decoder) {
            guard let frame = remaining.popFirst() else { throw PlugIPCError.disconnected }
            return frame
        }
        XCTAssertTrue(remaining.isEmpty)
        guard case let .tools(tools) = try decoder.decode(IPCResponse.self, from: payload) else { return XCTFail() }
        XCTAssertEqual(tools.count, 2)
    }
}
