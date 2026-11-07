package dev.sail.app.feature.dialogue

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.nio.ByteBuffer
import java.nio.ByteOrder

class WavTest {
    private fun wav(rate: Int, channels: Int, bits: Int, samples: ByteArray): ByteArray {
        val buf = ByteBuffer.allocate(44 + samples.size).order(ByteOrder.LITTLE_ENDIAN)
        buf.put("RIFF".toByteArray()).putInt(36 + samples.size).put("WAVE".toByteArray())
        buf.put("fmt ".toByteArray()).putInt(16).putShort(1).putShort(channels.toShort())
            .putInt(rate).putInt(rate * channels * bits / 8).putShort((channels * bits / 8).toShort()).putShort(bits.toShort())
        buf.put("data".toByteArray()).putInt(samples.size).put(samples)
        return buf.array()
    }

    @Test
    fun decodesPcm16() {
        val pcm = Wav.decode(wav(24_000, 1, 16, byteArrayOf(1, 0, 2, 0)))!!
        assertEquals(24_000, pcm.sampleRate)
        assertEquals(1, pcm.channels)
        assertArrayEquals(byteArrayOf(1, 0, 2, 0), pcm.data)
    }

    @Test
    fun rejectsOtherFormats() {
        assertNull(Wav.decode(wav(24_000, 1, 8, byteArrayOf(1, 2))))
        assertNull(Wav.decode(ByteArray(10)))
        assertNull(Wav.decode(ByteArray(64)))
    }
}
