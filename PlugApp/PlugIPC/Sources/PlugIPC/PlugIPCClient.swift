import Darwin
import Foundation

public actor PlugIPCClient {
    private var descriptor: Int32 = -1
    private let socketURL: URL
    private let clientVersion: String
    private let requestTimeout: TimeInterval
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    public init(
        socketURL: URL = PlugIPCClient.defaultSocketURL,
        clientVersion: String,
        requestTimeout: TimeInterval = 3
    ) {
        self.socketURL = socketURL
        self.clientVersion = clientVersion
        self.requestTimeout = min(60, max(0.001, requestTimeout))
        encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
    }

    // Keep package-level callers source-compatible while app callers pass the
    // installed bundle version explicitly.
    public init(
        socketURL: URL = PlugIPCClient.defaultSocketURL,
        requestTimeout: TimeInterval = 3
    ) {
        self.init(
            socketURL: socketURL,
            clientVersion: Self.bundleClientVersion,
            requestTimeout: requestTimeout
        )
    }

    private static var bundleClientVersion: String {
        clientVersion(from: Bundle.main.infoDictionary ?? [:])
    }

    public static func clientVersion(from infoDictionary: [String: Any]) -> String {
        guard let version = infoDictionary["CFBundleShortVersionString"] as? String else {
            return "development"
        }
        let trimmed = version.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "development" : trimmed
    }

    public static var defaultSocketURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Library/Application Support/plug/plug.sock")
    }

    public static var defaultTokenURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Library/Application Support/plug/plug.token")
    }

    public func connect() throws -> OperatorHandshake {
        if descriptor < 0 {
            descriptor = try Self.openSocket(path: socketURL.path, requestTimeout: requestTimeout)
        }
        let response = try request(.handshake(clientVersion: clientVersion, ipcMin: 3, ipcMax: 4))
        guard case let .handshake(handshake) = response else { throw PlugIPCError.unexpectedResponse("handshake") }
        return handshake
    }

    public func request(_ request: IPCRequest) throws -> IPCResponse {
        if descriptor < 0 {
            descriptor = try Self.openSocket(path: socketURL.path, requestTimeout: requestTimeout)
        }
        do {
            let deadline = Self.deadline(after: requestTimeout)
            try Self.writeAll(
                FrameCodec.encode(request, encoder: encoder),
                to: descriptor,
                deadline: deadline
            )
            let header = try Self.readExact(4, from: descriptor, deadline: deadline)
            let length = header.reduce(UInt32.zero) { ($0 << 8) | UInt32($1) }
            guard length <= FrameCodec.maximumPayloadSize else { throw PlugIPCError.frameTooLarge(Int(length)) }
            let payload = try Self.readExact(Int(length), from: descriptor, deadline: deadline)
            let response = try decoder.decode(IPCResponse.self, from: payload)
            if case let .error(code, message) = response { throw PlugIPCError.daemon(code, message) }
            return response
        } catch {
            disconnect()
            throw error
        }
    }

    public func disconnect() {
        if descriptor >= 0 { Darwin.close(descriptor); descriptor = -1 }
    }

    static func openSocket(path: String, requestTimeout: TimeInterval = 3) throws -> Int32 {
        let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw PlugIPCError.systemCall("socket", errno) }
        var noSigPipe: Int32 = 1
        guard Darwin.setsockopt(
            fd,
            SOL_SOCKET,
            SO_NOSIGPIPE,
            &noSigPipe,
            socklen_t(MemoryLayout<Int32>.size)
        ) == 0 else {
            let code = errno
            Darwin.close(fd)
            throw PlugIPCError.systemCall("setsockopt", code)
        }
        let flags = Darwin.fcntl(fd, F_GETFL)
        guard flags >= 0, Darwin.fcntl(fd, F_SETFL, flags | O_NONBLOCK) == 0 else {
            let code = errno
            Darwin.close(fd)
            throw PlugIPCError.systemCall("fcntl", code)
        }
        // Keep O_NONBLOCK: readiness polling cannot bound a subsequent blocking syscall.
        var (address, addressLength) = try unixSocketAddress(path: path)
        let result = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(fd, $0, addressLength)
            }
        }
        if result != 0 {
            guard errno == EINPROGRESS else {
                let code = errno
                Darwin.close(fd)
                throw PlugIPCError.systemCall("connect", code)
            }
            do {
                try waitForReadiness(
                    descriptor: fd,
                    events: Int16(POLLOUT),
                    deadline: deadline(after: requestTimeout)
                )
                var socketError: Int32 = 0
                var length = socklen_t(MemoryLayout<Int32>.size)
                guard Darwin.getsockopt(fd, SOL_SOCKET, SO_ERROR, &socketError, &length) == 0,
                      socketError == 0
                else {
                    throw PlugIPCError.systemCall("connect", socketError == 0 ? errno : socketError)
                }
            } catch {
                Darwin.close(fd)
                throw error
            }
        }
        return fd
    }

    static func unixSocketAddress(path: String) throws -> (sockaddr_un, socklen_t) {
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8CString)
        guard bytes.count <= MemoryLayout.size(ofValue: address.sun_path) else {
            throw PlugIPCError.socketPathTooLong
        }
        withUnsafeMutableBytes(of: &address.sun_path) { destination in
            bytes.withUnsafeBytes { source in
                destination.copyBytes(from: source)
            }
        }
        let headerLength = MemoryLayout<sockaddr_un>.offset(of: \sockaddr_un.sun_path)!
        let addressLength = headerLength + bytes.count
        address.sun_len = UInt8(addressLength)
        return (address, socklen_t(addressLength))
    }

    private static func writeAll(_ data: Data, to fd: Int32, deadline: UInt64) throws {
        try data.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                try waitForReadiness(
                    descriptor: fd,
                    events: Int16(POLLOUT),
                    deadline: deadline
                )
                let count = Darwin.write(fd, raw.baseAddress!.advanced(by: offset), raw.count - offset)
                if count > 0 {
                    offset += count
                    continue
                }
                let code = errno
                if code == EINTR || code == EAGAIN || code == EWOULDBLOCK { continue }
                throw PlugIPCError.systemCall("write", code)
            }
        }
    }

    private static func readExact(_ count: Int, from fd: Int32, deadline: UInt64) throws -> Data {
        var data = Data(count: count)
        try data.withUnsafeMutableBytes { raw in
            var offset = 0
            while offset < count {
                try waitForReadiness(
                    descriptor: fd,
                    events: Int16(POLLIN),
                    deadline: deadline
                )
                let readCount = Darwin.read(fd, raw.baseAddress!.advanced(by: offset), count - offset)
                if readCount > 0 {
                    offset += readCount
                    continue
                }
                if readCount == 0 { throw PlugIPCError.disconnected }
                let code = errno
                if code == EINTR || code == EAGAIN || code == EWOULDBLOCK { continue }
                throw PlugIPCError.systemCall("read", code)
            }
        }
        return data
    }

    private static func deadline(after timeout: TimeInterval) -> UInt64 {
        let nanoseconds = UInt64(timeout * 1_000_000_000)
        let sum = DispatchTime.now().uptimeNanoseconds.addingReportingOverflow(nanoseconds)
        return sum.overflow ? UInt64.max : sum.partialValue
    }

    private static func waitForReadiness(
        descriptor: Int32,
        events: Int16,
        deadline: UInt64
    ) throws {
        while true {
            let now = DispatchTime.now().uptimeNanoseconds
            guard now < deadline else { throw PlugIPCError.timedOut }
            let remaining = deadline - now
            let milliseconds = max(1, (remaining + 999_999) / 1_000_000)
            var item = pollfd(fd: descriptor, events: events, revents: 0)
            let result = Darwin.poll(
                &item,
                1,
                Int32(min(milliseconds, UInt64(Int32.max)))
            )
            if result > 0 { return }
            if result == 0 { throw PlugIPCError.timedOut }
            if errno != EINTR { throw PlugIPCError.systemCall("poll", errno) }
        }
    }
}
