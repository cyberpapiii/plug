import Darwin
import XCTest
@testable import PlugIPC

final class FrameCodecTests: XCTestCase {
    func testLengthPrefixedJSONRoundTrip() throws {
        let request = IPCRequest.handshake(clientVersion: "0.6.2", ipcMin: 3, ipcMax: 4)
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
}

private struct IPCRequestMirror: Decodable { let type: String }
