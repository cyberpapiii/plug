import Darwin
import Foundation
import XCTest
@testable import PlugIPC

/// A probe that forgets to close its socket leaks one descriptor per call, and
/// the app probes the daemon several times per reconcile.
final class ConnectionLifetimeTests: XCTestCase {
    func testHandshakeAndDisconnectClosesTheSocket() async throws {
        let server = try HandshakeThenWaitForCloseServer()
        defer { server.stop() }
        let client = PlugIPCClient(socketURL: server.socketURL, clientVersion: "test")

        let handshake = try await client.handshakeAndDisconnect()

        XCTAssertEqual(handshake.daemonVersion, "0.7.0")
        XCTAssertTrue(server.waitForPeerClose(), "the probe must close its descriptor")
    }

    func testReleasingAConnectedClientClosesTheSocket() async throws {
        let server = try HandshakeThenWaitForCloseServer()
        defer { server.stop() }
        var client: PlugIPCClient? = PlugIPCClient(socketURL: server.socketURL, clientVersion: "test")

        _ = try await client?.connect()
        XCTAssertFalse(server.waitForPeerClose(timeout: .milliseconds(100)), "still connected")
        client = nil

        XCTAssertTrue(server.waitForPeerClose(), "deinit must close the descriptor")
    }
}

/// Accepts one connection, answers its handshake, then reports when the
/// client closes its end.
private final class HandshakeThenWaitForCloseServer: @unchecked Sendable {
    let socketURL: URL
    private let listener: Int32
    private let peerClosed = DispatchSemaphore(value: 0)

    init() throws {
        socketURL = URL(fileURLWithPath: "/tmp")
            .appending(path: "plug-lifetime-\(UUID().uuidString).sock")
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

        let peerClosed = peerClosed
        DispatchQueue.global(qos: .userInitiated).async {
            let accepted = Darwin.accept(listenerFD, nil, nil)
            guard accepted >= 0 else { return }
            defer { Darwin.close(accepted) }
            guard let header = Self.readExact(accepted, count: 4) else { return }
            let length = header.reduce(UInt32.zero) { ($0 << 8) | UInt32($1) }
            guard Self.readExact(accepted, count: Int(length)) != nil else { return }
            let payload = Data("""
            {"type":"OperatorHandshake","handshake":{"daemon_version":"0.7.0","ipc_min":3,"ipc_max":6,"ownership":"app_managed","capabilities":[]}}
            """.utf8)
            var frame = Data([
                UInt8((payload.count >> 24) & 0xff), UInt8((payload.count >> 16) & 0xff),
                UInt8((payload.count >> 8) & 0xff), UInt8(payload.count & 0xff),
            ])
            frame.append(payload)
            frame.withUnsafeBytes { raw in _ = Darwin.write(accepted, raw.baseAddress!, raw.count) }
            var byte: UInt8 = 0
            while true {
                let count = Darwin.read(accepted, &byte, 1)
                if count == 0 { peerClosed.signal(); return }
                if count < 0, errno != EINTR { return }
            }
        }
    }

    /// True once the client has closed its end. Stays true on later calls.
    func waitForPeerClose(timeout: DispatchTimeInterval = .seconds(2)) -> Bool {
        guard peerClosed.wait(timeout: .now() + timeout) == .success else { return false }
        peerClosed.signal()
        return true
    }

    func stop() {
        Darwin.shutdown(listener, SHUT_RDWR)
        Darwin.close(listener)
        Darwin.unlink(socketURL.path)
    }

    private static func readExact(_ fd: Int32, count: Int) -> Data? {
        var data = Data(count: count)
        let ok = data.withUnsafeMutableBytes { raw -> Bool in
            var offset = 0
            while offset < count {
                let readCount = Darwin.read(fd, raw.baseAddress!.advanced(by: offset), count - offset)
                guard readCount > 0 else { return false }
                offset += readCount
            }
            return true
        }
        return ok ? data : nil
    }
}
