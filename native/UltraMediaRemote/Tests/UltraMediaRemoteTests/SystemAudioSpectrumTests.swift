// Pure-logic tests for the system-output spectrum analysis.
//
// License: MIT (see /LICENSE in the repository root)

import XCTest

@testable import UltraMediaRemote

final class SystemAudioSpectrumTests: XCTestCase {
    /// Renders a sine wave of the given frequency and amplitude.
    private func sine(
        frequency: Double,
        amplitude: Double,
        sampleRate: Double,
        count: Int
    ) -> [Float] {
        (0..<count).map { index in
            Float(amplitude * sin(2 * .pi * frequency * Double(index) / sampleRate))
        }
    }

    func testNormalizedLevelMapsDecibelWindow() {
        // Silence and non-finite inputs clamp to zero.
        XCTAssertEqual(SystemAudioSignal.normalizedLevel(rms: 0), 0)
        XCTAssertEqual(SystemAudioSignal.normalizedLevel(rms: -1), 0)
        XCTAssertEqual(SystemAudioSignal.normalizedLevel(rms: .nan), 0)

        // Full-scale sine RMS (~0.707) is ~-3 dB, close to the ceiling.
        XCTAssertGreaterThan(SystemAudioSignal.normalizedLevel(rms: 0.7071), 0.9)
        // -60 dB maps to the floor of the window.
        XCTAssertEqual(SystemAudioSignal.normalizedLevel(rms: 0.001), 0, accuracy: 0.01)
        // Values above full scale clamp to one.
        XCTAssertEqual(SystemAudioSignal.normalizedLevel(rms: 10), 1)
    }

    func testAccumulateSpectrumPeaksAtMatchingBand() {
        let sampleRate = 48_000.0
        let count = 8_192
        let amplitude = 0.25
        let samples = sine(
            frequency: 2_000, amplitude: amplitude,
            sampleRate: sampleRate, count: count)

        var spectrum = [Float](repeating: 0, count: SystemAudioSignal.bandCount)
        SystemAudioSignal.accumulateSpectrum(
            samples: samples, count: count, sampleRate: sampleRate, into: &spectrum)

        let bandIndex = SystemAudioSignal.bandFrequencies.firstIndex(of: 2_000)!
        for index in SystemAudioSignal.bandFrequencies.indices where index != bandIndex {
            XCTAssertLessThan(
                spectrum[index], spectrum[bandIndex],
                "band \(SystemAudioSignal.bandFrequencies[index]) must stay below the 2 kHz bin")
        }
        // The matched band responds near the expected normalized level.
        let expected = SystemAudioSignal.normalizedLevel(
            rms: amplitude / sqrt(2))
        XCTAssertEqual(Double(spectrum[bandIndex]), Double(expected), accuracy: 0.15)
    }

    func testAccumulateSpectrumRejectsInvalidInputs() {
        var spectrum = [Float](repeating: 0, count: SystemAudioSignal.bandCount)

        // Non-positive or non-finite sample rates are ignored.
        SystemAudioSignal.accumulateSpectrum(
            samples: [0, 0, 0], count: 3, sampleRate: 0, into: &spectrum)
        SystemAudioSignal.accumulateSpectrum(
            samples: [0, 0, 0], count: 3, sampleRate: .nan, into: &spectrum)
        XCTAssertTrue(spectrum.allSatisfy { $0 == 0 })

        // Bands above Nyquist are skipped rather than producing garbage: at a
        // 100 Hz rate every visual band exceeds Nyquist, so nothing accumulates.
        var shortRateSpectrum = [Float](repeating: 0, count: SystemAudioSignal.bandCount)
        SystemAudioSignal.accumulateSpectrum(
            samples: sine(frequency: 63, amplitude: 0.5, sampleRate: 100, count: 512),
            count: 512, sampleRate: 100, into: &shortRateSpectrum)
        XCTAssertTrue(shortRateSpectrum.allSatisfy { $0 == 0 })
    }

    func testStoreAppliesImmediateAttackAndShortRelease() throws {
        let store = SpectrumStore()

        store.set(spectrum: [Float](repeating: 1, count: SystemAudioSignal.bandCount))
        XCTAssertEqual(store.spectrum, [Float](repeating: 1, count: SystemAudioSignal.bandCount))

        // Falling targets decay toward the previous value instead of snapping.
        store.set(spectrum: [Float](repeating: 0, count: SystemAudioSignal.bandCount))
        let decayed = try XCTUnwrap(store.spectrum.first)
        XCTAssertEqual(Double(decayed), 0.68, accuracy: 0.001)

        // Out-of-range targets clamp into [0, 1].
        store.set(spectrum: [Float](repeating: -5, count: SystemAudioSignal.bandCount))
        for value in store.spectrum {
            XCTAssertGreaterThanOrEqual(value, 0)
            XCTAssertLessThanOrEqual(value, 1)
        }
    }
}
