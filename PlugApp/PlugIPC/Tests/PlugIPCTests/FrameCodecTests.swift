import Darwin
import XCTest
@testable import PlugIPC

final class FrameCodecTests: XCTestCase {
    func testBundleClientVersionFallsBackWhenMetadataMissing() {
        XCTAssertEqual(PlugIPCClient.clientVersion(from: [:]), "development")
        XCTAssertEqual(
            PlugIPCClient.clientVersion(from: ["CFBundleShortVersionString": "  "]),
            "development"
        )
        XCTAssertEqual(
            PlugIPCClient.clientVersion(from: ["CFBundleShortVersionString": "0.7.0"]),
            "0.7.0"
        )
    }

    func testLengthPrefixedJSONRoundTrip() throws {
        let request = IPCRequest.handshake(clientVersion: "0.6.4", ipcMin: 3, ipcMax: 4)
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let frame = try FrameCodec.encode(request, encoder: encoder)
        XCTAssertEqual(frame.prefix(4), Data([0, 0, 0, UInt8(frame.count - 4)]))
        XCTAssertEqual(try FrameCodec.decode(IPCRequestMirror.self, from: frame, decoder: decoder).type, "OperatorHandshake")
    }

    func testUnixSocketConnectUsesDarwinAddressLayout() throws {
        let path = "/tmp/plug-ipc-\(UUID().uuidString).sock"
        let server = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        XCTAssertGreaterThanOrEqual(server, 0)
        defer {
            Darwin.close(server)
            Darwin.unlink(path)
        }

        var (address, addressLength) = try PlugIPCClient.unixSocketAddress(path: path)
        XCTAssertEqual(address.sun_family, sa_family_t(AF_UNIX))
        XCTAssertEqual(Int(address.sun_len), Int(addressLength))
        let bindResult = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(server, $0, addressLength)
            }
        }
        XCTAssertEqual(bindResult, 0, "bind failed with errno \(errno)")
        XCTAssertEqual(Darwin.listen(server, 1), 0)

        let client = try PlugIPCClient.openSocket(path: path)
        defer { Darwin.close(client) }
        let accepted = Darwin.accept(server, nil, nil)
        XCTAssertGreaterThanOrEqual(accepted, 0)
        if accepted >= 0 { Darwin.close(accepted) }
    }

    func testOperatorSnapshotDecodesSnakeCaseIdentifiers() throws {
        let payload = Data(#"""
        {
          "type": "OperatorSnapshot",
          "snapshot": {
            "runtime_version": "0.5.3",
            "uptime_secs": 12,
            "ownership": "app_managed",
            "configured_servers": [],
            "servers": [{"server_id":"alpha","health":"Healthy","tool_count":3}],
            "live_sessions": [{
              "transport":"daemon_proxy","client_id":"client-1","session_id":"session-1",
              "client_type":"CodexCli","client_info":"codex","connected_secs":4,
              "last_activity_secs":null
            }],
            "client_visibility": [{
              "session_id":"session-1","client_type":"CodexCli","visible_tool_count":3
            }],
            "upstream_auth": [],
            "downstream_clients": [{
              "client_id":"remote-1","client_name":"Remote","redirect_uris":[],
              "source":"dynamic_registration"
            }]
          }
        }
        """#.utf8)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase

        guard case let .snapshot(snapshot) = try decoder.decode(IPCResponse.self, from: payload) else {
            return XCTFail("snapshot response expected")
        }
        XCTAssertEqual(snapshot.servers.first?.serverId, "alpha")
        XCTAssertEqual(snapshot.liveSessions.first?.clientId, "client-1")
        XCTAssertEqual(snapshot.liveSessions.first?.sessionId, "session-1")
        XCTAssertEqual(snapshot.clientVisibility.first?.sessionId, "session-1")
        XCTAssertEqual(snapshot.downstreamClients.first?.clientId, "remote-1")
    }

    func testServerConfigRequestAndResponseRoundTripAdvancedFields() throws {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let request = IPCRequest.serverConfig(authToken: "secret", name: "workspace")
        let frame = try FrameCodec.encode(request, encoder: encoder)
        let decodedRequest = try JSONDecoder().decode(
            IPCRequestMirror.self,
            from: frame.dropFirst(4)
        )
        XCTAssertEqual(decodedRequest.type, "GetServerConfig")

        let payload = Data(#"""
        {
          "type":"ServerConfig",
          "name":"workspace",
          "server":{
            "command":"uvx",
            "args":["workspace-mcp"],
            "env":{"API_KEY":"kept"},
            "enabled":true,
            "transport":"stdio",
            "protocol":"legacy",
            "timeout_secs":30,
            "call_timeout_secs":300,
            "max_concurrent":1,
            "health_check_interval_secs":60,
            "circuit_breaker_enabled":true,
            "enrichment":false,
            "tool_renames":{},
            "tool_groups":[{"prefix":"Gmail","contains":["gmail"],"strip":[]}],
            "sandbox":{"enabled":true,"allow_network":false,"allow_read":["/tmp"],"allow_write":[]}
          }
        }
        """#.utf8)
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        guard case let .serverConfig(name, server) = try decoder.decode(IPCResponse.self, from: payload) else {
            return XCTFail("server config response expected")
        }
        XCTAssertEqual(name, "workspace")
        XCTAssertEqual(server.env["API_KEY"], "kept")
        XCTAssertEqual(server.toolGroups.first?.prefix, "Gmail")
        XCTAssertEqual(server.sandbox?.allowRead, ["/tmp"])
    }

    func testHandshakeTimesOutWhenSocketAcceptsButNeverReplies() async throws {
        let server = try AcceptWithoutReplyServer()
        defer { server.stop() }
        let client = PlugIPCClient(socketURL: server.socketURL, requestTimeout: 0.15)
        let started = ContinuousClock.now

        do {
            _ = try await client.connect()
            XCTFail("Expected bounded IPC timeout")
        } catch let error as PlugIPCError {
            XCTAssertEqual(error, .timedOut)
        } catch {
            XCTFail("Unexpected error: \(error)")
        }

        XCTAssertLessThan(started.duration(to: .now), .seconds(1))
    }

    func testLargeRequestTimesOutWhenAcceptedSocketNeverReads() async throws {
        let server = try AcceptedNeverReadServer()
        defer { server.stop() }
        let client = PlugIPCClient(socketURL: server.socketURL, requestTimeout: 0.15)
        let request = IPCRequest.addServer(
            authToken: "token",
            name: "large",
            server: .command("/bin/true", args: [String(repeating: "x", count: 3_900_000)])
        )
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let frame = try FrameCodec.encode(request, encoder: encoder)
        XCTAssertGreaterThan(frame.count, 3_000_000)

        let started = ContinuousClock.now
        do {
            _ = try await client.request(request)
            XCTFail("Expected bounded IPC write timeout")
        } catch let error as PlugIPCError {
            XCTAssertEqual(error, .timedOut)
        } catch {
            XCTFail("Unexpected error: \(error)")
        }

        XCTAssertLessThan(started.duration(to: .now), .milliseconds(500))
    }
}

private struct IPCRequestMirror: Decodable { let type: String }

private final class AcceptWithoutReplyServer: @unchecked Sendable {
    let socketURL: URL
    private let listener: Int32
    private let release = DispatchSemaphore(value: 0)
    private let finished = DispatchSemaphore(value: 0)

    init() throws {
        socketURL = URL(fileURLWithPath: "/tmp")
            .appending(path: "plug-stale-ipc-\(UUID().uuidString).sock")
        listener = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard listener >= 0 else { throw PlugIPCError.systemCall("socket", errno) }
        Darwin.unlink(socketURL.path)
        let listenerFD = listener
        var (address, addressLength) = try PlugIPCClient.unixSocketAddress(path: socketURL.path)
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(listenerFD, $0, addressLength)
            }
        }
        guard bound == 0, Darwin.listen(listenerFD, 1) == 0 else {
            let code = errno
            Darwin.close(listener)
            throw PlugIPCError.systemCall("listen", code)
        }

        let release = release
        let finished = finished
        DispatchQueue.global(qos: .userInitiated).async {
            let accepted = Darwin.accept(listenerFD, nil, nil)
            if accepted >= 0 {
                release.wait()
                Darwin.close(accepted)
            }
            finished.signal()
        }
    }

    func stop() {
        release.signal()
        _ = finished.wait(timeout: .now() + 1)
        Darwin.close(listener)
        Darwin.unlink(socketURL.path)
    }
}

private final class AcceptedNeverReadServer: @unchecked Sendable {
    let socketURL: URL
    private let listener: Int32
    private let accepted = DispatchSemaphore(value: 0)
    private let release = DispatchSemaphore(value: 0)
    private let finished = DispatchSemaphore(value: 0)

    init() throws {
        socketURL = URL(fileURLWithPath: "/tmp")
            .appending(path: "plug-blocked-write-\(UUID().uuidString).sock")
        listener = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard listener >= 0 else { throw PlugIPCError.systemCall("socket", errno) }
        Darwin.unlink(socketURL.path)
        let listenerFD = listener
        var (address, addressLength) = try PlugIPCClient.unixSocketAddress(path: socketURL.path)
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(listenerFD, $0, addressLength)
            }
        }
        guard bound == 0, Darwin.listen(listenerFD, 1) == 0 else {
            let code = errno
            Darwin.close(listener)
            throw PlugIPCError.systemCall("listen", code)
        }

        // Establish a first connection so readiness is signaled only after accept
        // succeeds. The second connection is the request socket that never reads.
        let readinessClient = try PlugIPCClient.openSocket(path: socketURL.path)
        let release = release
        let finished = finished
        let accepted = accepted
        DispatchQueue.global(qos: .userInitiated).async {
            let readinessSocket = Darwin.accept(listenerFD, nil, nil)
            if readinessSocket >= 0 {
                accepted.signal()
                Darwin.close(readinessSocket)
            }

            let requestSocket = Darwin.accept(listenerFD, nil, nil)
            if requestSocket >= 0 {
                _ = release.wait(timeout: .now() + .milliseconds(750))
                Darwin.close(requestSocket)
            }
            finished.signal()
        }
        guard accepted.wait(timeout: .now() + 1) == .success else {
            Darwin.close(readinessClient)
            Darwin.close(listener)
            _ = finished.wait(timeout: .now() + 1)
            throw PlugIPCError.systemCall("accept", ETIMEDOUT)
        }
        Darwin.close(readinessClient)
    }

    func stop() {
        release.signal()
        _ = finished.wait(timeout: .now() + 1)
        Darwin.close(listener)
        Darwin.unlink(socketURL.path)
    }
}
