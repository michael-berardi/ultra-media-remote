/*
 *  UltraMediaRemote.swift
 *  Runtime-only access to the private macOS MediaRemote framework.
 *
 *  Exposes @_cdecl functions (umr_*) for Rust callers:
 *    - Now Playing metadata as a JSON string
 *    - Media transport control (play/pause, next, previous)
 *    - Change subscriptions via polling
 *
 *  Nothing is statically linked: MediaRemote is opened with dlopen/dlsym at
 *  runtime. When the framework or its symbols are absent the controller
 *  degrades to unavailable and never fabricates results.
 *
 *  License: MIT (see /LICENSE in the repository root)
 */

import AppKit
import Foundation

// MARK: - Pure mapping helpers

/// `MRMediaRemoteCommand` values used by `MRMediaRemoteSendCommand`.
internal enum MediaTransportCode {
    static let playPause: UInt32 = 2
    static let next: UInt32 = 4
    static let previous: UInt32 = 5
}

private enum NowPlayingKey {
    static let title = "kMRMediaRemoteNowPlayingInfoTitle"
    static let artist = "kMRMediaRemoteNowPlayingInfoArtist"
    static let album = "kMRMediaRemoteNowPlayingInfoAlbum"
    static let elapsedTime = "kMRMediaRemoteNowPlayingInfoElapsedTime"
    static let duration = "kMRMediaRemoteNowPlayingInfoDuration"
    static let playbackRate = "kMRMediaRemoteNowPlayingInfoPlaybackRate"
    static let applicationIsPlaying = "kMRMediaRemoteNowPlayingApplicationIsPlaying"
}

internal struct NowPlayingSnapshot: Equatable, Sendable {
    let processID: pid_t?
    let appName: String?
    let bundleIdentifier: String?
    let title: String?
    let artist: String?
    let album: String?
    let elapsedSeconds: Double?
    let durationSeconds: Double?
    let isPlaying: Bool?

    /// Playback rate above zero means playing; falls back to the explicit
    /// playing flag when the rate key is absent.
    static func playingState(from info: [String: Any]) -> Bool? {
        if let rate = info[NowPlayingKey.playbackRate] as? Double {
            return rate > 0
        }
        if let rate = info[NowPlayingKey.playbackRate] as? NSNumber {
            return rate.doubleValue > 0
        }
        if let playing = info[NowPlayingKey.applicationIsPlaying] as? Bool {
            return playing
        }
        return nil
    }

    static func time(_ key: String, from info: [String: Any]) -> Double? {
        let value: Double
        if let number = info[key] as? Double {
            value = number
        } else if let number = info[key] as? NSNumber {
            value = number.doubleValue
        } else {
            return nil
        }
        return value.isFinite && value >= 0 ? value : nil
    }

    /// Wire payload with snake_case keys matching the Rust `NowPlaying` struct.
    var jsonPayload: [String: Any] {
        var payload: [String: Any] = [:]
        if let processID { payload["pid"] = Int(processID) }
        if let appName { payload["app_name"] = appName }
        if let bundleIdentifier { payload["bundle_id"] = bundleIdentifier }
        if let title { payload["title"] = title }
        if let artist { payload["artist"] = artist }
        if let album { payload["album"] = album }
        if let elapsedSeconds { payload["elapsed_seconds"] = elapsedSeconds }
        if let durationSeconds { payload["duration_seconds"] = durationSeconds }
        if let isPlaying { payload["is_playing"] = isPlaying }
        return payload
    }

    func jsonString() -> String? {
        guard let data = try? JSONSerialization.data(
            withJSONObject: jsonPayload, options: [.sortedKeys])
        else { return nil }
        return String(data: data, encoding: .utf8)
    }
}

// MARK: - Thread-safe handoff boxes

private final class NowPlayingBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: NowPlayingSnapshot?

    func set(_ newValue: NowPlayingSnapshot?) {
        lock.lock()
        value = newValue
        lock.unlock()
    }

    func get() -> NowPlayingSnapshot? {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}

/// Thread-safe handoff box for the async now-playing PID callback.
private final class NowPlayingPIDBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: pid_t?

    func set(_ newValue: pid_t) {
        lock.lock()
        value = newValue
        lock.unlock()
    }

    func get() -> pid_t? {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}

private final class SupportedCommandsBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Set<UInt32>?

    func set(_ newValue: Set<UInt32>) {
        lock.lock()
        value = newValue
        lock.unlock()
    }

    func get() -> Set<UInt32>? {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}

// MARK: - Runtime-only MediaRemote access

/// Runtime-only access to the private MediaRemote framework via dlopen/dlsym.
/// Nothing is statically linked; when the framework or symbols are absent the
/// controller degrades to unavailable and callers must not fake results.
internal final class MediaRemoteController: @unchecked Sendable {
    static let shared = MediaRemoteController()

    private typealias SendCommandFn = @convention(c) (UInt32, AnyObject?) -> UInt8
    private typealias GetNowPlayingInfoFn = @convention(c) (
        DispatchQueue, @escaping @convention(block) (AnyObject?) -> Void
    ) -> Void
    private typealias GetNowPlayingApplicationPIDFn = @convention(c) (
        DispatchQueue, @escaping @convention(block) (pid_t) -> Void
    ) -> Void
    private typealias CopySupportedCommandsFn = @convention(c) (
        DispatchQueue, @escaping @convention(block) (AnyObject?) -> Void
    ) -> Void
    private typealias CommandInfoGetCommandFn = @convention(c) (AnyObject) -> UInt32
    private typealias CommandInfoGetEnabledFn = @convention(c) (AnyObject) -> UInt8
    private typealias RegisterForNotificationsFn = @convention(c) (DispatchQueue) -> Void
    private typealias SetWantsNotificationsFn = @convention(c) (UInt8) -> Void

    private let sendCommand: SendCommandFn?
    private let getNowPlayingInfo: GetNowPlayingInfoFn?
    private let getNowPlayingApplicationPID: GetNowPlayingApplicationPIDFn?
    private let copySupportedCommands: CopySupportedCommandsFn?
    private let commandInfoGetCommand: CommandInfoGetCommandFn?
    private let commandInfoGetEnabled: CommandInfoGetEnabledFn?

    private init() {
        guard let handle = dlopen(
            "/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote", RTLD_LAZY)
        else {
            sendCommand = nil
            getNowPlayingInfo = nil
            getNowPlayingApplicationPID = nil
            copySupportedCommands = nil
            commandInfoGetCommand = nil
            commandInfoGetEnabled = nil
            return
        }
        sendCommand = dlsym(handle, "MRMediaRemoteSendCommand").map {
            unsafeBitCast($0, to: SendCommandFn.self)
        }
        getNowPlayingInfo = dlsym(handle, "MRMediaRemoteGetNowPlayingInfo").map {
            unsafeBitCast($0, to: GetNowPlayingInfoFn.self)
        }
        getNowPlayingApplicationPID = dlsym(
            handle, "MRMediaRemoteGetNowPlayingApplicationPID"
        ).map {
            unsafeBitCast($0, to: GetNowPlayingApplicationPIDFn.self)
        }
        copySupportedCommands = dlsym(handle, "MRMediaRemoteCopySupportedCommands").map {
            unsafeBitCast($0, to: CopySupportedCommandsFn.self)
        }
        commandInfoGetCommand = dlsym(
            handle, "MRMediaRemoteCommandInfoGetCommand"
        ).map {
            unsafeBitCast($0, to: CommandInfoGetCommandFn.self)
        }
        commandInfoGetEnabled = dlsym(
            handle, "MRMediaRemoteCommandInfoGetEnabled"
        ).map {
            unsafeBitCast($0, to: CommandInfoGetEnabledFn.self)
        }
        // Notifications keep cached metadata fresh; both are optional.
        if let symbol = dlsym(handle, "MRMediaRemoteRegisterForNowPlayingNotifications") {
            let register = unsafeBitCast(symbol, to: RegisterForNotificationsFn.self)
            register(DispatchQueue.main)
        }
        if let symbol = dlsym(handle, "MRMediaRemoteSetWantsNowPlayingNotifications") {
            let setWants = unsafeBitCast(symbol, to: SetWantsNotificationsFn.self)
            setWants(1)
        }
    }

    /// Transport delivery is independent of metadata availability.
    var transportAvailable: Bool {
        sendCommand != nil
    }

    /// Metadata reading depends on the now-playing info symbol.
    var nowPlayingAvailable: Bool {
        getNowPlayingInfo != nil
    }

    /// Sends a raw MediaRemote command code and reports MediaRemote's UInt8
    /// Boolean result.
    func send(code: UInt32) -> Bool {
        guard let send = sendCommand else { return false }
        return send(code, nil) != 0
    }

    /// Reads enabled transport commands through MediaRemote's asynchronous
    /// supported-command callback. Missing symbols or timeout safely yield an
    /// empty set, disabling secondary controls.
    func enabledCommandCodes(timeout: TimeInterval = 0.5) -> Set<UInt32> {
        guard let copySupportedCommands,
            let getCommand = commandInfoGetCommand,
            let getEnabled = commandInfoGetEnabled
        else { return [] }
        let box = SupportedCommandsBox()
        let semaphore = DispatchSemaphore(value: 0)
        copySupportedCommands(DispatchQueue.global(qos: .userInitiated)) { commands in
            defer { semaphore.signal() }
            guard let array = commands as? [AnyObject] else { return }
            box.set(Set(array.compactMap { info in
                getEnabled(info) != 0 ? getCommand(info) : nil
            }))
        }
        guard semaphore.wait(timeout: .now() + timeout) == .success else { return [] }
        return box.get() ?? []
    }

    /// Fetches the now-playing client PID without requiring metadata.
    func nowPlayingProcessID(timeout: TimeInterval = 0.25) -> pid_t? {
        guard let getPID = getNowPlayingApplicationPID else { return nil }
        let pidBox = NowPlayingPIDBox()
        let group = DispatchGroup()
        group.enter()
        getPID(DispatchQueue.global(qos: .userInitiated)) {
            pidBox.set($0)
            group.leave()
        }
        guard group.wait(timeout: .now() + timeout) == .success else { return nil }
        return pidBox.get().flatMap { $0 > 0 ? $0 : nil }
    }

    /// Fetches canonical now-playing metadata. The owning PID is best-effort:
    /// some browser sessions expose metadata but withhold or delay the PID.
    /// Metadata remains usable because MediaRemote itself is the system's
    /// canonical now-playing session.
    func nowPlayingSnapshot(timeout: TimeInterval = 0.75) -> NowPlayingSnapshot? {
        guard let getInfo = getNowPlayingInfo else { return nil }
        let infoBox = NowPlayingBox()
        let pidBox = NowPlayingPIDBox()
        let semaphore = DispatchSemaphore(value: 0)
        let queue = DispatchQueue.global(qos: .userInitiated)

        getInfo(queue) { info in
            defer { semaphore.signal() }
            guard let dict = info as? [String: Any] else { return }
            infoBox.set(NowPlayingSnapshot(
                processID: nil,
                appName: nil,
                bundleIdentifier: nil,
                title: dict[NowPlayingKey.title] as? String,
                artist: dict[NowPlayingKey.artist] as? String,
                album: dict[NowPlayingKey.album] as? String,
                elapsedSeconds: NowPlayingSnapshot.time(NowPlayingKey.elapsedTime, from: dict),
                durationSeconds: NowPlayingSnapshot.time(NowPlayingKey.duration, from: dict),
                isPlaying: NowPlayingSnapshot.playingState(from: dict)))
        }

        getNowPlayingApplicationPID?(queue) { pid in
            pidBox.set(pid)
        }

        guard semaphore.wait(timeout: .now() + timeout) == .success,
            let info = infoBox.get()
        else { return nil }
        let processID = pidBox.get().flatMap { $0 > 0 ? $0 : nil }
        let app = processID.flatMap(NSRunningApplication.init(processIdentifier:))
        return NowPlayingSnapshot(
            processID: processID,
            appName: app?.localizedName,
            bundleIdentifier: app?.bundleIdentifier,
            title: info.title,
            artist: info.artist,
            album: info.album,
            elapsedSeconds: info.elapsedSeconds,
            durationSeconds: info.durationSeconds,
            isPlaying: info.isPlaying)
    }
}

// MARK: - Polling subscription

/// One active polling subscription running on its own thread + RunLoop.
/// Delivers the JSON payload whenever it changes, including changes to/from
/// nil (nothing playing). The C-string passed to the callback is valid only
/// for the duration of the call.
internal final class NowPlayingSubscription: @unchecked Sendable {
    private let interval: TimeInterval
    private let deliver: @Sendable (String?) -> Void
    private let lock = NSLock()
    private var cancelled = false
    private var lastDelivered: String?

    init(interval: TimeInterval, deliver: @escaping @Sendable (String?) -> Void) {
        self.interval = max(interval, 0.05)
        self.deliver = deliver
    }

    func start() {
        let subscription = self
        let thread = Thread {
            let runLoop = RunLoop.current
            let timer = Timer(timeInterval: subscription.interval, repeats: true) { _ in
                subscription.poll()
            }
            runLoop.add(timer, forMode: .default)
            subscription.poll()
            while !subscription.isCancelled {
                runLoop.run(mode: .default, before: Date(timeIntervalSinceNow: 0.5))
            }
            timer.invalidate()
        }
        thread.name = "UltraMediaRemoteSubscription"
        thread.qualityOfService = .utility
        thread.start()
    }

    private var isCancelled: Bool {
        lock.lock()
        defer { lock.unlock() }
        return cancelled
    }

    func stop() {
        lock.lock()
        cancelled = true
        lock.unlock()
    }

    private func poll() {
        let snapshot = MediaRemoteController.shared.nowPlayingSnapshot()
        deliver(snapshot?.jsonString())
    }

    /// Delivers only when the payload changed since the previous poll.
    func deliverIfChanged(_ json: String?) {
        lock.lock()
        if json == lastDelivered {
            lock.unlock()
            return
        }
        lastDelivered = json
        lock.unlock()
        deliver(json)
    }
}

internal final class SubscriptionRegistry: @unchecked Sendable {
    static let shared = SubscriptionRegistry()

    private let lock = NSLock()
    private var subscriptions: [UInt64: NowPlayingSubscription] = [:]
    private var nextHandle: UInt64 = 1

    func add(_ subscription: NowPlayingSubscription) -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        let handle = nextHandle
        nextHandle += 1
        subscriptions[handle] = subscription
        return handle
    }

    func remove(_ handle: UInt64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard let subscription = subscriptions.removeValue(forKey: handle) else { return false }
        subscription.stop()
        return true
    }
}

// MARK: - Helpers

extension String {
    internal func duplicateAsCChar() -> UnsafeMutablePointer<CChar> {
        guard let duplicate = withCString({ strdup($0) }) else {
            fatalError("Unable to allocate C string")
        }
        return duplicate
    }
}

// MARK: - Public C ABI

/// Returns 1 when runtime MediaRemote metadata access is available, 0 otherwise.
@_cdecl("umr_now_playing_available")
public func umr_now_playing_available() -> Int32 {
    MediaRemoteController.shared.nowPlayingAvailable ? 1 : 0
}

/// Returns 1 when runtime MediaRemote transport delivery is available.
@_cdecl("umr_transport_available")
public func umr_transport_available() -> Int32 {
    MediaRemoteController.shared.transportAvailable ? 1 : 0
}

/// Fetches now-playing metadata and returns it as an allocated JSON string
/// (snake_case keys: pid, app_name, bundle_id, title, artist, album,
/// elapsed_seconds, duration_seconds, is_playing). Returns NULL when
/// MediaRemote is unavailable or the reply did not arrive in time. The caller
/// frees the string with `umr_free_string`.
@_cdecl("umr_now_playing_fetch")
public func umr_now_playing_fetch(_ timeoutMs: UInt32) -> UnsafeMutablePointer<CChar>? {
    let timeout = max(0, Double(timeoutMs) / 1000.0)
    guard let snapshot = MediaRemoteController.shared.nowPlayingSnapshot(timeout: timeout),
        let json = snapshot.jsonString()
    else { return nil }
    return json.duplicateAsCChar()
}

/// Writes the enabled+supported transport command codes into `codesOut` and
/// returns how many were written (truncated to `capacity`). Always writes 0
/// when discovery APIs are missing; the caller decides what that implies.
@_cdecl("umr_transport_supported_commands")
public func umr_transport_supported_commands(
    _ codesOut: UnsafeMutablePointer<UInt32>?,
    _ capacity: UInt32
) -> Int32 {
    guard let codesOut, capacity > 0 else { return 0 }
    let codes = MediaRemoteController.shared.enabledCommandCodes().sorted()
    let count = min(Int(capacity), codes.count)
    for index in 0..<count {
        codesOut[index] = codes[index]
    }
    return Int32(count)
}

/// Sends a raw MediaRemote transport command code.
/// Returns 1 when sent, 0 when unavailable/failed.
@_cdecl("umr_transport_send")
public func umr_transport_send(_ code: UInt32) -> Int32 {
    MediaRemoteController.shared.send(code: code) ? 1 : 0
}

/// Starts polling-based now-playing updates. `callback` receives a context
/// value plus a JSON C string that is valid only during the call; the string
/// is NULL when nothing is playing. Delivery happens on a dedicated thread.
/// Returns a nonzero handle on success, 0 on failure.
@_cdecl("umr_now_playing_subscribe")
public func umr_now_playing_subscribe(
    _ callback: (@convention(c) (UInt64, UnsafePointer<CChar>?) -> Void)?,
    _ context: UInt64,
    _ intervalMs: UInt32
) -> UInt64 {
    guard let callback else { return 0 }
    let interval = max(50, Double(intervalMs)) / 1000.0
    let subscription = NowPlayingSubscription(interval: interval) { json in
        json?.withCString { pointer in
            callback(context, pointer)
        } ?? callback(context, nil)
    }
    let handle = SubscriptionRegistry.shared.add(subscription)
    subscription.start()
    return handle
}

/// Stops and releases a subscription created by `umr_now_playing_subscribe`.
@_cdecl("umr_now_playing_unsubscribe")
public func umr_now_playing_unsubscribe(_ handle: UInt64) {
    _ = SubscriptionRegistry.shared.remove(handle)
}

/// Frees a string previously returned by this library.
@_cdecl("umr_free_string")
public func umr_free_string(_ s: UnsafeMutablePointer<CChar>?) {
    if let s = s {
        free(s)
    }
}

