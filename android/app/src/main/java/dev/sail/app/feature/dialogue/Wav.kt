package dev.sail.app.feature.dialogue

import java.nio.ByteBuffer
import java.nio.ByteOrder

/** 16-bit PCM audio from a WAV file (what SAIL's TTS sends in "audio_chunk"). */
class Pcm16(val sampleRate: Int, val channels: Int, val data: ByteArray)

object Wav {
    /** Returns null for anything that isn't little-endian 16-bit PCM WAV. */
    fun decode(bytes: ByteArray): Pcm16? {
        if (bytes.size < 44) return null
        val buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        if (tag(bytes, 0) != "RIFF" || tag(bytes, 8) != "WAVE") return null
        var offset = 12
        var sampleRate = 0
        var channels = 0
        var bits = 0
        var format = 0
        while (offset + 8 <= bytes.size) {
            val id = tag(bytes, offset)
            val size = buf.getInt(offset + 4)
            val body = offset + 8
            if (size < 0 || body + size > bytes.size && id != "data") return null
            when (id) {
                "fmt " -> {
                    format = buf.getShort(body).toInt()
                    channels = buf.getShort(body + 2).toInt()
                    sampleRate = buf.getInt(body + 4)
                    bits = buf.getShort(body + 14).toInt()
                }
                "data" -> {
                    if (format != 1 || bits != 16 || channels !in 1..2 || sampleRate <= 0) return null
                    val end = minOf(bytes.size, body + size)
                    return Pcm16(sampleRate, channels, bytes.copyOfRange(body, end))
                }
            }
            offset = body + size + (size and 1)
        }
        return null
    }

    private fun tag(bytes: ByteArray, at: Int) = String(bytes, at, 4, Charsets.US_ASCII)
}
