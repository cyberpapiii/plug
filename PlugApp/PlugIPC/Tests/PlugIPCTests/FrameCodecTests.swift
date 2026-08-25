import XCTest
@testable import PlugIPC

final class FrameCodecTests: XCTestCase {
    func testLengthPrefixedJSONRoundTrip() throws {
        let request = IPCRequest.handshake(clientVersion: "0.5.2", ipcMin: 3, ipcMax: 4)
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let frame = try FrameCodec.encode(request, encoder: encoder)
        XCTAssertEqual(frame.prefix(4), Data([0, 0, 0, UInt8(frame.count - 4)]))
        XCTAssertEqual(try FrameCodec.decode(IPCRequestMirror.self, from: frame, decoder: decoder).type, "OperatorHandshake")
    }
}

private struct IPCRequestMirror: Decodable { let type: String }
