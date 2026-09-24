import XCTest
@testable import PlugIPC

/// Contract tests for shared `testdata/ipc/*.json` fixtures serialized by Rust.
final class GoldenPayloadTests: XCTestCase {
    private let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    private func goldenData(_ name: String, file: StaticString = #filePath, line: UInt = #line) throws -> Data {
        let repoRoot = URL(fileURLWithPath: "\(file)")
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        let url = repoRoot.appending(path: "testdata/ipc/\(name)")
        guard FileManager.default.fileExists(atPath: url.path) else {
            struct MissingGolden: Error {}
            XCTFail("missing shared golden fixture at \(url.path)", file: file, line: line)
            throw MissingGolden()
        }
        return try Data(contentsOf: url)
    }

    private func goldenJSON(_ name: String, file: StaticString = #filePath, line: UInt = #line) throws -> (Data, [String: Any]) {
        let data = try goldenData(name, file: file, line: line)
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            XCTFail("golden \(name) must be a JSON object", file: file, line: line)
            return (data, [:])
        }
        return (data, object)
    }

    func testGoldenOperatorHandshakeResponseDecodesInSwift() throws {
        let (payload, object) = try goldenJSON("operator_handshake_response.json")
        try assertExactKeys(
            object,
            expected: ["type", "handshake"],
            context: "operator handshake response"
        )

        guard case let .handshake(handshake) = try decoder.decode(IPCResponse.self, from: payload) else {
            return XCTFail("expected OperatorHandshake IPCResponse variant")
        }

        XCTAssertEqual(handshake.daemonVersion, "0.7.0")
        XCTAssertEqual(handshake.ipcMin, 3)
        XCTAssertEqual(handshake.ipcMax, 6)
        XCTAssertEqual(handshake.ownership, "app_managed")
        XCTAssertTrue(handshake.capabilities.contains("server_config_read"))
        XCTAssertTrue(handshake.capabilities.contains("server_mutation"))
    }

    func testGoldenGetServerConfigRequestDecodesThroughFrameCodec() throws {
        let (payload, object) = try goldenJSON("get_server_config_request.json")
        try assertExactKeys(
            object,
            expected: ["type", "auth_token", "name"],
            context: "GetServerConfig request"
        )

        var frame = Data()
        var length = UInt32(payload.count).bigEndian
        frame.append(Data(bytes: &length, count: MemoryLayout<UInt32>.size))
        frame.append(payload)

        let decoded = try FrameCodec.decode(GetServerConfigRequestFixture.self, from: frame, decoder: decoder)
        XCTAssertEqual(decoded.type, "GetServerConfig")
        XCTAssertEqual(decoded.authToken, "golden-auth-token")
        XCTAssertEqual(decoded.name, "workspace")
    }

    func testGoldenGetServerConfigResponseDecodesInSwift() throws {
        let (payload, object) = try goldenJSON("get_server_config_response.json")
        try assertExactKeys(
            object,
            expected: ["type", "name", "server"],
            context: "ServerConfig response"
        )

        guard case let .serverConfig(name, server) = try decoder.decode(IPCResponse.self, from: payload) else {
            return XCTFail("expected ServerConfig IPCResponse variant")
        }

        XCTAssertEqual(name, "workspace")
        XCTAssertEqual(server.command, "uvx")
        XCTAssertEqual(server.args, ["workspace-mcp"])
        XCTAssertEqual(server.transport, "stdio")
    }

    /// The snapshot the app polls every two seconds, as Rust writes it, with
    /// one server in every daemon health. The app maps these health strings
    /// to what a person reads, so a renamed variant must fail here first.
    func testGoldenOperatorSnapshotDecodesEveryServerHealthInSwift() throws {
        let (payload, object) = try goldenJSON("operator_snapshot_response.json")
        try assertExactKeys(object, expected: ["type", "snapshot"], context: "OperatorSnapshot response")

        guard case let .snapshot(snapshot) = try decoder.decode(IPCResponse.self, from: payload) else {
            return XCTFail("expected OperatorSnapshot IPCResponse variant")
        }

        XCTAssertEqual(snapshot.runtimeVersion, "0.7.0")
        XCTAssertEqual(snapshot.toolCatalogRevision, 7)
        XCTAssertEqual(snapshot.ownership, "app_managed")
        XCTAssertEqual(snapshot.configuredServers.map(\.name), ["healthy", "degraded", "failed", "auth-required"])
        XCTAssertEqual(snapshot.configuredServers.map(\.transport), ["stdio", "stdio", "stdio", "stdio"])
        XCTAssertEqual(snapshot.servers.map(\.serverId), ["healthy", "degraded", "failed", "auth-required"])
        XCTAssertEqual(snapshot.servers.map(\.health), ["Healthy", "Degraded", "Failed", "AuthRequired"])
        XCTAssertEqual(snapshot.servers.map(\.toolCount), [12, 4, 0, 0])
        XCTAssertEqual(snapshot.liveSessions.first?.transport, "daemon_proxy")
        XCTAssertEqual(snapshot.liveSessions.first?.clientType, "ClaudeCode")
        XCTAssertEqual(snapshot.clientVisibility.first?.visibleToolCount, 16)
        XCTAssertEqual(snapshot.upstreamAuth.first?.health, "AuthRequired")
        XCTAssertEqual(snapshot.upstreamAuth.first?.authenticated, false)
        XCTAssertEqual(snapshot.downstreamClients.first?.source, "dynamic_registration")
    }

    func testRenamedHandshakeFieldFailsExactKeyContract() throws {
        let (_, object) = try goldenJSON("operator_handshake_response.json")
        guard var handshake = object["handshake"] as? [String: Any] else {
            return XCTFail("handshake object expected")
        }

        try assertExactKeys(
            handshake,
            expected: [
                "daemon_version",
                "ipc_min",
                "ipc_max",
                "ownership",
                "capabilities",
            ],
            context: "operator handshake body"
        )

        handshake.removeValue(forKey: "daemon_version")
        handshake["version"] = "0.7.0"
        var mutatedObject = object
        mutatedObject["handshake"] = handshake
        let mutated = try JSONSerialization.data(withJSONObject: mutatedObject)

        XCTAssertThrowsError(try decoder.decode(IPCResponse.self, from: mutated)) { error in
            XCTAssertTrue(error is DecodingError, "renamed snake_case field must fail Swift decode")
        }
    }

    private func assertExactKeys(
        _ object: [String: Any],
        expected: Set<String>,
        context: String,
        file: StaticString = #filePath,
        line: UInt = #line
    ) throws {
        let actual = Set(object.keys)
        XCTAssertEqual(
            actual,
            expected,
            "unexpected JSON keys in \(context); rename drift should fail CI",
            file: file,
            line: line
        )
    }
}

private struct GetServerConfigRequestFixture: Decodable {
    let type: String
    let authToken: String
    let name: String
}
