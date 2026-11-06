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
        let frame = try FrameCodec.encode(IPCRequest.status)
        XCTAssertEqual(frame.prefix(4), Data([0, 0, 0, UInt8(frame.count - 4)]))
        XCTAssertEqual(frame.dropFirst(4), Data(#"{"type":"Status"}"#.utf8))
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

    func testHandshakeTimesOutWhenSocketAcceptsButNeverReplies() async throws {
        let server = try AcceptWithoutReplyServer()
        defer { server.stop() }
        let client = PlugIPCClient(socketURL: server.socketURL, clientVersion: "0.7.0", requestTimeout: 0.15)
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
        let client = PlugIPCClient(socketURL: server.socketURL, clientVersion: "0.7.0", requestTimeout: 0.15)
        let request = IPCRequest.operator(token: "token", .mutate(.addServer(
            name: "large",
            server: .command("/bin/true", args: [String(repeating: "x", count: 3_900_000)])
        )))
        let frame = try FrameCodec.encode(request, encoder: PlugIPCClient.makeEncoder())
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

    private func chunkFrames(_ payload: Data, pieceSize: Int) -> [Data] {
        let pieces = stride(from: 0, to: payload.count, by: pieceSize).map {
            payload.subdata(in: $0..<min($0 + pieceSize, payload.count))
        }
        return pieces.enumerated().map { index, piece in
            Data("""
            {"envelope":"ResponseChunk","chunk_index":\(index),"chunk_count":\(pieces.count),"payload_b64":"\(piece.base64EncodedString())"}
            """.utf8)
        }
    }

    private func readPayload(_ frames: [Data]) throws -> Data {
        var remaining = frames[...]
        return try PlugIPCClient.readResponsePayload(decoder: PlugIPCClient.makeDecoder()) {
            guard let frame = remaining.popFirst() else { throw PlugIPCError.disconnected }
            return frame
        }
    }

    func testUnchunkedResponsePassesThrough() throws {
        let payload = Data(#"{"type":"Ok"}"#.utf8)
        XCTAssertEqual(try readPayload([payload]), payload)
    }

    func testOutOfOrderChunksAreRejected() {
        let frames = chunkFrames(Data(#"{"type":"Ok"}"#.utf8), pieceSize: 4)
        XCTAssertThrowsError(try readPayload([frames[1], frames[0]] + frames.dropFirst(2)))
        XCTAssertThrowsError(try readPayload([frames[0], Data(#"{"type":"Ok"}"#.utf8)]))
    }
}

private final class AcceptWithoutReplyServer: @unchecked Sendable {
    let socketURL = TestSocket.uniquePath("plug-stale-ipc")
    private let listener: Int32
    private let release = DispatchSemaphore(value: 0)
    private let finished = DispatchSemaphore(value: 0)

    init() throws {
        listener = try TestSocket.listen(at: socketURL.path)
        let listenerFD = listener
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
    let socketURL = TestSocket.uniquePath("plug-blocked-write")
    private let listener: Int32
    private let accepted = DispatchSemaphore(value: 0)
    private let release = DispatchSemaphore(value: 0)
    private let finished = DispatchSemaphore(value: 0)

    init() throws {
        listener = try TestSocket.listen(at: socketURL.path)
        let listenerFD = listener

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
