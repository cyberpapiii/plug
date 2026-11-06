import Darwin
import Foundation

/// A Unix socket and the length-prefixed frames the daemon speaks, for tests
/// that stand in for the daemon. Both test targets compile this one file.
enum TestSocket {
    struct Failure: Error, Equatable {
        let call: String
        let code: Int32
    }

    /// A socket path no other test is using. Under /tmp: a test's own
    /// temporary directory can be longer than `sun_path` allows.
    static func uniquePath(_ prefix: String) -> URL {
        URL(fileURLWithPath: "/tmp/\(prefix)-\(UUID().uuidString).sock")
    }

    /// A listening socket at `path`, replacing whatever was there.
    static func listen(at path: String, backlog: Int32 = 1) throws -> Int32 {
        let descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else { throw Failure(call: "socket", code: errno) }
        Darwin.unlink(path)
        let bound = try withAddress(path) { Darwin.bind(descriptor, $0, $1) }
        guard bound == 0, Darwin.listen(descriptor, backlog) == 0 else {
            let code = errno
            Darwin.close(descriptor)
            throw Failure(call: "listen", code: code)
        }
        return descriptor
    }

    /// Connects to `path` and returns the descriptor, or -1.
    @discardableResult
    static func connect(to path: String) -> Int32 {
        let descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else { return -1 }
        let connected = (try? withAddress(path) { Darwin.connect(descriptor, $0, $1) }) ?? -1
        guard connected == 0 else {
            Darwin.close(descriptor)
            return -1
        }
        return descriptor
    }

    static func readExact(_ descriptor: Int32, count: Int) -> Data? {
        var data = Data(count: count)
        let complete = data.withUnsafeMutableBytes { raw -> Bool in
            var offset = 0
            while offset < count {
                let read = Darwin.read(descriptor, raw.baseAddress!.advanced(by: offset), count - offset)
                if read > 0 {
                    offset += read
                } else if read < 0, errno == EINTR {
                    continue
                } else {
                    return false
                }
            }
            return true
        }
        return complete ? data : nil
    }

    /// One frame's payload, or nil once the peer has gone.
    static func readFrame(_ descriptor: Int32) -> Data? {
        guard let header = readExact(descriptor, count: 4) else { return nil }
        let length = header.reduce(UInt32.zero) { ($0 << 8) | UInt32($1) }
        return readExact(descriptor, count: Int(length))
    }

    /// One frame read as a JSON object.
    static func readRequest(_ descriptor: Int32) -> [String: Any]? {
        readFrame(descriptor).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
    }

    static func writeFrame(_ payload: Data, to descriptor: Int32) {
        var length = UInt32(payload.count).bigEndian
        var frame = Data(bytes: &length, count: MemoryLayout<UInt32>.size)
        frame.append(payload)
        frame.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                let written = Darwin.write(descriptor, raw.baseAddress!.advanced(by: offset), raw.count - offset)
                if written > 0 {
                    offset += written
                } else if written < 0, errno == EINTR {
                    continue
                } else {
                    return
                }
            }
        }
    }

    static func writeJSON(_ object: [String: Any], to descriptor: Int32) {
        guard let payload = try? JSONSerialization.data(withJSONObject: object) else { return }
        writeFrame(payload, to: descriptor)
    }

    private static func withAddress(
        _ path: String,
        _ body: (UnsafePointer<sockaddr>, socklen_t) -> Int32
    ) throws -> Int32 {
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8CString)
        guard bytes.count <= MemoryLayout.size(ofValue: address.sun_path) else {
            throw Failure(call: "sun_path", code: ENAMETOOLONG)
        }
        withUnsafeMutableBytes(of: &address.sun_path) { destination in
            bytes.withUnsafeBytes { source in destination.copyBytes(from: source) }
        }
        let length = MemoryLayout<sockaddr_un>.offset(of: \sockaddr_un.sun_path)! + bytes.count
        address.sun_len = UInt8(length)
        return withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { body($0, socklen_t(length)) }
        }
    }
}
