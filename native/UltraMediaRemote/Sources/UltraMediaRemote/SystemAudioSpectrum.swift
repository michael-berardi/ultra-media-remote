/*
 *  SystemAudioSpectrum.swift
 *  Opt-in live system-output spectrum via a ScreenCaptureKit audio tap.
 *
 *  Ports the proven UltraVox meter design: PCM from an SCStream audio output
 *  is analyzed with fixed-bin Goertzel magnitudes at the visual band
 *  frequencies, normalized from RMS to [0, 1] over a -60 dB..0 dB window, and
 *  smoothed with immediate attack / short release. No decorative curve is
 *  applied on top; every value derives exclusively from PCM.
 *
 *  Requires macOS 15+ and Screen Recording permission for the calling app.
 *
 *  License: MIT (see /LICENSE in the repository root)
 */

// Compiled only when the Rust `spectrum` cargo feature enables it; see build.rs.
#if UMR_SPECTRUM

import Accelerate
import AVFoundation
import AudioToolbox
import CoreGraphics
import CoreMedia
import Foundation
@preconcurrency import ScreenCaptureKit

// MARK: - Pure analysis helpers

/// Frequency bins used by the UI, ordered low to high. Each value comes from
/// the corresponding PCM frequency magnitude; no decorative bar curve is
/// applied later in the frontend.
enum SystemAudioSignal {
    static let bandFrequencies: [Double] = [
        63, 125, 250, 500, 1_000, 2_000, 3_500, 5_000, 7_500, 10_000, 14_000,
    ]

    static let bandCount: Int = bandFrequencies.count

    static func normalizedLevel(rms: Double) -> Float {
        guard rms.isFinite, rms > 0 else { return 0 }
        let decibels = 20 * log10(rms)
        return Float(min(1, max(0, (decibels + 60) / 60)))
    }

    /// Accumulates real Goertzel magnitudes for the fixed visual frequency
    /// bins. This avoids an FFT allocation/setup on every ScreenCaptureKit
    /// callback while preserving frequency-selective response.
    static func accumulateSpectrum(
        samples: UnsafePointer<Float>,
        count: Int,
        sampleRate: Double,
        into spectrum: inout [Float]
    ) {
        guard count > 0, sampleRate.isFinite, sampleRate > 0,
            spectrum.count == bandCount
        else { return }

        for (bandIndex, frequency) in bandFrequencies.enumerated() {
            guard frequency < sampleRate / 2 else { continue }
            let coefficient = 2 * cos(2 * Double.pi * frequency / sampleRate)
            var previous = 0.0
            var previousPrevious = 0.0
            for sampleIndex in 0..<count {
                let current =
                    Double(samples[sampleIndex]) + coefficient * previous - previousPrevious
                previousPrevious = previous
                previous = current
            }
            let power = max(
                0,
                previous * previous + previousPrevious * previousPrevious
                    - coefficient * previous * previousPrevious
            )
            let amplitude = 2 * sqrt(power) / Double(count)
            spectrum[bandIndex] = max(
                spectrum[bandIndex],
                normalizedLevel(rms: amplitude)
            )
        }
    }
}

// MARK: - Smoothing store

/// Per-session store applying immediate attack, short release smoothing so
/// visuals do not chatter while every value still derives from PCM only.
final class SpectrumStore: @unchecked Sendable {
    private let lock = NSLock()
    private var storedSpectrum = [Float](repeating: 0, count: SystemAudioSignal.bandCount)

    var spectrum: [Float] {
        lock.withLock { storedSpectrum }
    }

    func set(spectrum: [Float]) {
        lock.withLock {
            guard spectrum.count == storedSpectrum.count else { return }
            for index in storedSpectrum.indices {
                let target = min(1, max(0, spectrum[index]))
                // Immediate attack, short release.
                storedSpectrum[index] =
                    target >= storedSpectrum[index]
                    ? target
                    : storedSpectrum[index] * 0.68 + target * 0.32
            }
        }
    }

    func ingest(_ buffer: AVAudioPCMBuffer) {
        guard let channels = buffer.floatChannelData else { return }
        let format = buffer.format
        let channelCount = Int(format.channelCount)
        let sampleCount = Int(buffer.frameLength)
        guard channelCount > 0, sampleCount > 0 else { return }
        var spectrum = [Float](repeating: 0, count: SystemAudioSignal.bandCount)
        for channelIndex in 0..<channelCount {
            SystemAudioSignal.accumulateSpectrum(
                samples: channels[channelIndex],
                count: sampleCount,
                sampleRate: format.sampleRate,
                into: &spectrum
            )
        }
        set(spectrum: spectrum)
    }

    func reset() {
        lock.withLock {
            storedSpectrum = [Float](repeating: 0, count: SystemAudioSignal.bandCount)
        }
    }
}

// MARK: - SCStream audio output

@available(macOS 15.0, *)
private final class SpectrumOutput: NSObject, SCStreamOutput, @unchecked Sendable {
    let store = SpectrumStore()

    func stream(
        _ stream: SCStream,
        didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of outputType: SCStreamOutputType
    ) {
        guard outputType == .audio,
            sampleBuffer.isValid,
            let description = sampleBuffer.formatDescription
        else { return }
        let format = AVAudioFormat(cmAudioFormatDescription: description)

        let frameCount = AVAudioFrameCount(sampleBuffer.numSamples)
        guard frameCount > 0,
            let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frameCount)
        else { return }
        buffer.frameLength = frameCount
        guard CMSampleBufferCopyPCMDataIntoAudioBufferList(
            sampleBuffer,
            at: 0,
            frameCount: Int32(frameCount),
            into: buffer.mutableAudioBufferList
        ) == noErr else { return }
        store.ingest(buffer)
    }
}

// MARK: - Session lifecycle

/// Thread-safe handoff for one async result read from a blocked thread.
private final class AsyncResultBox<T: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var result: Result<T, Error>?

    func set(_ value: Result<T, Error>) {
        lock.lock()
        result = value
        lock.unlock()
    }

    func get() -> Result<T, Error>? {
        lock.lock()
        defer { lock.unlock() }
        return result
    }
}

private func runAsyncAndBlock<T: Sendable>(
    _ operation: @Sendable @escaping () async throws -> T
) throws -> T {
    let box = AsyncResultBox<T>()
    let semaphore = DispatchSemaphore(value: 0)
    let task = Task {
        defer { semaphore.signal() }
        do {
            box.set(.success(try await operation()))
        } catch {
            box.set(.failure(error))
        }
    }
    semaphore.wait()
    switch box.get() {
    case .success(let value): return value
    case .failure(let error): throw error
    case .none:
        task.cancel()
        throw NSError(
            domain: "UltraMediaRemote", code: 1,
            userInfo: [NSLocalizedDescriptionKey: "spectrum start did not complete"])
    }
}

protocol SpectrumCaptureSession: AnyObject, Sendable {
    func stop()
    func spectrumJSON() -> String?
}

/// One active system-audio capture session feeding its own smoothing store.
/// Each handle owns a dedicated SCStream; callers normally open exactly one.
@available(macOS 15.0, *)
final class SpectrumSession: SpectrumCaptureSession, @unchecked Sendable {
    private let output = SpectrumOutput()
    private let sampleQueue = DispatchQueue(
        label: "dev.implose.ultramediaremote.spectrum",
        qos: .userInteractive
    )
    private let lock = NSLock()
    private var stream: SCStream?

    func start() throws {
        lock.lock()
        defer { lock.unlock() }
        guard stream == nil else { return }
        let filter = try runAsyncAndBlock {
            let content = try await SCShareableContent.excludingDesktopWindows(
                false, onScreenWindowsOnly: true)
            guard let display = content.displays.first else {
                throw NSError(
                    domain: "UltraMediaRemote", code: 2,
                    userInfo: [NSLocalizedDescriptionKey: "no display available"])
            }
            return SCContentFilter(display: display, excludingWindows: [])
        }
        let configuration = SCStreamConfiguration()
        configuration.width = 16
        configuration.height = 16
        configuration.minimumFrameInterval = CMTime(value: 1, timescale: 1)
        configuration.queueDepth = 1
        configuration.showsCursor = false
        configuration.capturesAudio = true
        configuration.excludesCurrentProcessAudio = true
        configuration.captureMicrophone = false

        let stream = SCStream(filter: filter, configuration: configuration, delegate: nil)
        try stream.addStreamOutput(output, type: .audio, sampleHandlerQueue: sampleQueue)
        self.stream = stream
        do {
            try runAsyncAndBlock { try await stream.startCapture() }
        } catch {
            self.stream = nil
            output.store.reset()
            throw error
        }
    }

    func stop() {
        lock.lock()
        let stream = self.stream
        self.stream = nil

        guard let stream else {
            output.store.reset()
            return
        }
        try? runAsyncAndBlock { try await stream.stopCapture() }
        try? stream.removeStreamOutput(output, type: .audio)
        output.store.reset()
    }

    func spectrumJSON() -> String? {
        let values = output.store.spectrum.map { Double($0) }
        guard let data = try? JSONSerialization.data(withJSONObject: values) else { return nil }
        return String(data: data, encoding: .utf8)
    }
}

/// CoreAudio process taps capture the global output mix without coupling the
/// visualizer to screen frames. This is the durable-consent migration path for
/// hosts whose ScreenCaptureKit preflight lags an already-enabled app entry.
@available(macOS 15.0, *)
private final class CoreAudioSpectrumSession: SpectrumCaptureSession, @unchecked Sendable {
    private let store = SpectrumStore()
    private let sampleQueue = DispatchQueue(
        label: "dev.implose.ultramediaremote.coreaudio-spectrum",
        qos: .userInteractive
    )
    private let lock = NSLock()
    private var tapID = AudioObjectID(0)
    private var aggregateDeviceID = AudioObjectID(0)
    private var ioProcID: AudioDeviceIOProcID?

    func start() throws {
        lock.lock()
        defer { lock.unlock() }
        guard tapID == 0 else { return }

        let tapDescription = CATapDescription(stereoGlobalTapButExcludeProcesses: [])
        tapDescription.uuid = UUID()
        tapDescription.muteBehavior = .unmuted
        var createdTapID = AudioObjectID(0)
        var status = AudioHardwareCreateProcessTap(tapDescription, &createdTapID)
        guard status == noErr else {
            throw audioError("process tap creation", status)
        }
        tapID = createdTapID

        do {
            let outputDeviceID = try defaultSystemOutputDevice()
            let outputUID = try deviceUID(outputDeviceID)
            let aggregateDescription: [String: Any] = [
                kAudioAggregateDeviceNameKey: "UltraMediaRemote Spectrum",
                kAudioAggregateDeviceUIDKey: UUID().uuidString,
                kAudioAggregateDeviceMainSubDeviceKey: outputUID,
                kAudioAggregateDeviceIsPrivateKey: true,
                kAudioAggregateDeviceIsStackedKey: false,
                kAudioAggregateDeviceTapAutoStartKey: true,
                kAudioAggregateDeviceSubDeviceListKey: [
                    [kAudioSubDeviceUIDKey: outputUID]
                ],
                kAudioAggregateDeviceTapListKey: [
                    [
                        kAudioSubTapDriftCompensationKey: true,
                        kAudioSubTapUIDKey: tapDescription.uuid.uuidString,
                    ]
                ],
            ]
            status = AudioHardwareCreateAggregateDevice(
                aggregateDescription as CFDictionary,
                &aggregateDeviceID
            )
            guard status == noErr else {
                throw audioError("aggregate device creation", status)
            }

            var streamDescription = try tapStreamDescription(tapID)
            guard let format = AVAudioFormat(streamDescription: &streamDescription) else {
                throw audioError("tap format creation", kAudio_ParamError)
            }
            status = AudioDeviceCreateIOProcIDWithBlock(
                &ioProcID,
                aggregateDeviceID,
                sampleQueue
            ) { [weak self] _, inputData, _, _, _ in
                guard let self,
                    let buffer = AVAudioPCMBuffer(
                        pcmFormat: format,
                        bufferListNoCopy: inputData,
                        deallocator: nil
                    )
                else { return }
                self.store.ingest(buffer)
            }
            guard status == noErr else {
                throw audioError("I/O callback creation", status)
            }
            status = AudioDeviceStart(aggregateDeviceID, ioProcID)
            guard status == noErr else {
                throw audioError("aggregate device start", status)
            }
        } catch {
            releaseResources()
            throw error
        }
    }

    func stop() {
        lock.lock()
        defer { lock.unlock() }
        releaseResources()
    }

    func spectrumJSON() -> String? {
        let values = store.spectrum.map { Double($0) }
        guard let data = try? JSONSerialization.data(withJSONObject: values) else { return nil }
        return String(data: data, encoding: .utf8)
    }

    private func releaseResources() {
        if aggregateDeviceID != 0 {
            _ = AudioDeviceStop(aggregateDeviceID, ioProcID)
            if let ioProcID {
                _ = AudioDeviceDestroyIOProcID(aggregateDeviceID, ioProcID)
                self.ioProcID = nil
            }
            _ = AudioHardwareDestroyAggregateDevice(aggregateDeviceID)
            aggregateDeviceID = 0
        }
        if tapID != 0 {
            _ = AudioHardwareDestroyProcessTap(tapID)
            tapID = 0
        }
        store.reset()
    }

    deinit {
        releaseResources()
    }
}

private func audioError(_ operation: String, _ status: OSStatus) -> NSError {
    NSError(
        domain: "UltraMediaRemote.CoreAudio",
        code: Int(status),
        userInfo: [NSLocalizedDescriptionKey: "\(operation) failed with OSStatus \(status)"]
    )
}

private func defaultSystemOutputDevice() throws -> AudioObjectID {
    var address = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyDefaultSystemOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    var deviceID = AudioObjectID(0)
    var size = UInt32(MemoryLayout<AudioObjectID>.size)
    let status = AudioObjectGetPropertyData(
        AudioObjectID(kAudioObjectSystemObject),
        &address,
        0,
        nil,
        &size,
        &deviceID
    )
    guard status == noErr, deviceID != 0 else {
        throw audioError("default output lookup", status)
    }
    return deviceID
}

private func deviceUID(_ deviceID: AudioObjectID) throws -> String {
    var address = AudioObjectPropertyAddress(
        mSelector: kAudioDevicePropertyDeviceUID,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    var value: CFString?
    var size = UInt32(MemoryLayout<CFString?>.size)
    let status = withUnsafeMutablePointer(to: &value) { pointer in
        AudioObjectGetPropertyData(deviceID, &address, 0, nil, &size, pointer)
    }
    guard status == noErr, let value else {
        throw audioError("output UID lookup", status)
    }
    return value as String
}

private func tapStreamDescription(_ tapID: AudioObjectID) throws -> AudioStreamBasicDescription {
    var address = AudioObjectPropertyAddress(
        mSelector: kAudioTapPropertyFormat,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    var description = AudioStreamBasicDescription()
    var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
    let status = AudioObjectGetPropertyData(tapID, &address, 0, nil, &size, &description)
    guard status == noErr else {
        throw audioError("tap format lookup", status)
    }
    return description
}

/// Registry of active spectrum sessions, keyed by handle.
@available(macOS 15.0, *)
internal final class SpectrumRegistry: @unchecked Sendable {
    static let shared = SpectrumRegistry()

    private let lock = NSLock()
    private var sessions: [UInt64: any SpectrumCaptureSession] = [:]
    private var nextHandle: UInt64 = 1

    func add(_ session: any SpectrumCaptureSession) -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        let handle = nextHandle
        nextHandle += 1
        sessions[handle] = session
        return handle
    }

    func lookup(_ handle: UInt64) -> (any SpectrumCaptureSession)? {
        lock.lock()
        defer { lock.unlock() }
        return sessions[handle]
    }

    func remove(_ handle: UInt64) -> (any SpectrumCaptureSession)? {
        lock.lock()
        defer { lock.unlock() }
        return sessions.removeValue(forKey: handle)
    }
}


// MARK: - Public C ABI

/// Returns 1 when Screen Recording permission is already granted for the
/// calling process. This is a non-prompting preflight.
@_cdecl("umr_spectrum_permission_granted")
public func umr_spectrum_permission_granted() -> Int32 {
    CGPreflightScreenCaptureAccess() ? 1 : 0
}

/// Explicitly requests Screen Recording permission for the calling process.
/// This may show the system consent prompt and must only follow a user action.
@_cdecl("umr_spectrum_request_permission")
public func umr_spectrum_request_permission() -> Int32 {
    CGRequestScreenCaptureAccess() ? 1 : 0
}

@available(macOS 15.0, *)
private func startSpectrumSession() -> UInt64 {
    let session = SpectrumSession()
    do {
        try session.start()
    } catch {
        return 0
    }
    return SpectrumRegistry.shared.add(session)
}

/// Starts capturing system-output audio for spectral analysis. Returns a
/// nonzero handle on success, 0 when unsupported, permission is not already
/// granted, or capture could not start. This API never prompts for access.
@_cdecl("umr_spectrum_start")
public func umr_spectrum_start() -> UInt64 {
    guard #available(macOS 15.0, *), CGPreflightScreenCaptureAccess() else { return 0 }
    return startSpectrumSession()
}

/// Starts the global CoreAudio output tap after the host app confirms its
/// durable, previously recorded EQ consent.
@_cdecl("umr_spectrum_start_with_existing_consent")
public func umr_spectrum_start_with_existing_consent() -> UInt64 {
    guard #available(macOS 15.0, *) else { return 0 }
    let session = CoreAudioSpectrumSession()
    do {
        try session.start()
    } catch {
        return 0
    }
    return SpectrumRegistry.shared.add(session)
}

/// Returns the latest 11-band spectrum as a JSON array of normalized levels
/// in [0, 1], ordered low to high frequency (63 Hz .. 14 kHz). Returns NULL
/// for unknown handles. The caller frees the string with `umr_free_string`.
@_cdecl("umr_spectrum_fetch")
public func umr_spectrum_fetch(_ handle: UInt64) -> UnsafeMutablePointer<CChar>? {
    guard #available(macOS 15.0, *) else { return nil }
    guard let session = SpectrumRegistry.shared.lookup(handle),
        let json = session.spectrumJSON()
    else { return nil }
    return json.duplicateAsCChar()
}

/// Stops a spectrum session and releases its capture resources.
@_cdecl("umr_spectrum_stop")
public func umr_spectrum_stop(_ handle: UInt64) {
    guard #available(macOS 15.0, *) else { return }
    SpectrumRegistry.shared.remove(handle)?.stop()
}

#endif
