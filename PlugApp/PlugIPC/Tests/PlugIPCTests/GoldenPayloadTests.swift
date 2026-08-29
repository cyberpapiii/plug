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
            throw XCTSkip("missing shared golden fixture at \(url.path)")
        }
        return try Data(contentsOf: url)
    }

    private func goldenJSONObject(_ name: String) throws -> [String: Any] {
        let data = try goldenData(name)
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            XCTFail("golden \(name) must be a JSON object")
            return [:]
        }
        return object
    }

    func testGoldenOperatorHandshakeResponseDecodesInSwift() throws {
        let payload = try goldenData("operator_handshake_response.json")
        try assertExactKeys(
            try goldenJSONObject("operator_handshake_response.json"),
            expected: ["type", "handshake"],
            context: "operator handshake response"
        )

        guard case let .handshake(handshake) = try decoder.decode(IPCResponse.self, from: payload) else {
            return XCTFail("expected OperatorHandshake IPCResponse variant")
        }

        XCTAssertEqual(handshake.daemonVersion, "0.7.0")
        XCTAssertEqual(handshake.ipcMin, 3)
        XCTAssertEqual(handshake.ipcMax, 6)
        XCTAssertEqual(handshake.ownership, "unknown")
        XCTAssertTrue(handshake.stale)
        XCTAssertTrue(handshake.capabilities.contains("server_config_read"))
        XCTAssertTrue(handshake.capabilities.contains("server_mutation"))
    }

    func testGoldenGetServerConfigRequestDecodesThroughFrameCodec() throws {
        let payload = try goldenData("get_server_config_request.json")
        try assertExactKeys(
            try goldenJSONObject("get_server_config_request.json"),
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
        let payload = try goldenData("get_server_config_response.json")
        try assertExactKeys(
            try goldenJSONObject("get_server_config_response.json"),
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

    func testRenamedHandshakeFieldFailsExactKeyContract() throws {
        var object = try goldenJSONObject("operator_handshake_response.json")
        guard var handshake = object["handshake"] as? [String: Any] else {
            return XCTFail("handshake object expected")
        }

        handshake.removeValue(forKey: "daemon_version")
        handshake["version"] = "0.7.0"
        object["handshake"] = handshake
        let mutated = try JSONSerialization.data(withJSONObject: object)

        XCTAssertThrowsError(try decoder.decode(IPCResponse.self, from: mutated)) { error in
            XCTAssertTrue(error is DecodingError, "renamed snake_case field must fail Swift decode")
        }

        let handshakeObject = try goldenJSONObject("operator_handshake_response.json")["handshake"] as! [String: Any]
        try assertExactKeys(
            handshakeObject,
            expected: [
                "daemon_version",
                "ipc_min",
                "ipc_max",
                "ownership",
                "stale",
                "capabilities",
            ],
            context: "operator handshake body"
        )
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
