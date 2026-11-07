package dev.sail.app.feature.dialogue

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.NoiseSuppressor
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Microphone capture in the format /ws/voice expects: 16 kHz mono PCM16 little-endian, in
 * frames of 512 samples (1024 bytes), the same as the web client's capture.js. Runs on its
 * own thread; [onFrame] is called from that thread.
 */
class MicCapture(private val onFrame: (ByteArray) -> Unit, private val onError: (String) -> Unit) {
    private val running = AtomicBoolean(false)
    private var thread: Thread? = null

    val isRunning: Boolean get() = running.get()

    /** The caller must hold RECORD_AUDIO; the screen asks for it before calling this. */
    @SuppressLint("MissingPermission")
    fun start(): Boolean {
        if (!running.compareAndSet(false, true)) return true
        val minBuffer = AudioRecord.getMinBufferSize(SAMPLE_RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        if (minBuffer <= 0) {
            running.set(false)
            onError("This phone can't record 16 kHz audio")
            return false
        }
        val record = try {
            AudioRecord(
                MediaRecorder.AudioSource.VOICE_COMMUNICATION,
                SAMPLE_RATE,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
                maxOf(minBuffer, FRAME_BYTES * 8),
            )
        } catch (error: Exception) {
            running.set(false)
            onError("Microphone unavailable: ${error.message}")
            return false
        }
        if (record.state != AudioRecord.STATE_INITIALIZED) {
            record.release()
            running.set(false)
            onError("Microphone unavailable")
            return false
        }
        // Keep SAIL's own voice out of the capture where the phone supports it.
        val effects = listOfNotNull(
            if (AcousticEchoCanceler.isAvailable()) AcousticEchoCanceler.create(record.audioSessionId) else null,
            if (NoiseSuppressor.isAvailable()) NoiseSuppressor.create(record.audioSessionId) else null,
        )
        effects.forEach { it.enabled = true }
        thread = Thread({
            val frame = ByteArray(FRAME_BYTES)
            try {
                record.startRecording()
                while (running.get()) {
                    var filled = 0
                    while (filled < FRAME_BYTES && running.get()) {
                        val read = record.read(frame, filled, FRAME_BYTES - filled)
                        if (read < 0) {
                            onError("Microphone stopped (error $read)")
                            running.set(false)
                            break
                        }
                        filled += read
                    }
                    if (filled == FRAME_BYTES) onFrame(frame.copyOf())
                }
            } finally {
                runCatching { record.stop() }
                effects.forEach { it.release() }
                record.release()
            }
        }, "sail-mic").apply { start() }
        return true
    }

    fun stop() {
        running.set(false)
        thread = null
    }

    companion object {
        const val SAMPLE_RATE = 16_000
        const val FRAME_BYTES = 1024
    }
}
