package dev.sail.app.feature.dialogue

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.util.Base64
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.atomic.AtomicInteger

/**
 * Plays SAIL's TTS "audio_chunk" WAVs in order on one AudioTrack per sample rate. [stop]
 * drops everything queued (barge-in, interrupt). [onDrained] fires on the player thread
 * when the queue is empty after playing something, so the caller can send
 * "playback_finished".
 */
class SpeechPlayer(private val onDrained: () -> Unit, private val onError: (String) -> Unit) {
    private sealed interface Item {
        class Audio(val generation: Int, val pcm: Pcm16) : Item
        data object Quit : Item
    }

    private val queue = LinkedBlockingQueue<Item>()
    private val generation = AtomicInteger(0)
    private val pending = AtomicInteger(0)
    private val worker = Thread(::playLoop, "sail-speech").apply { isDaemon = true; start() }

    val isPlaying: Boolean get() = pending.get() > 0

    fun enqueue(wavBase64: String) {
        val bytes = try {
            Base64.decode(wavBase64, Base64.DEFAULT)
        } catch (error: IllegalArgumentException) {
            onError("SAIL sent audio this app can't decode")
            return
        }
        val pcm = Wav.decode(bytes) ?: return onError("SAIL sent audio in a format this app can't play")
        pending.incrementAndGet()
        queue.put(Item.Audio(generation.get(), pcm))
    }

    /** Stops playback now and forgets what's queued. */
    fun stop() {
        generation.incrementAndGet()
    }

    fun release() {
        stop()
        queue.put(Item.Quit)
    }

    private fun playLoop() {
        var track: AudioTrack? = null
        var trackRate = 0
        var trackChannels = 0
        try {
            while (true) {
                val item = queue.take()
                if (item is Item.Quit) return
                item as Item.Audio
                try {
                    if (item.generation != generation.get()) continue
                    val pcm = item.pcm
                    if (track == null || trackRate != pcm.sampleRate || trackChannels != pcm.channels) {
                        track?.release()
                        track = newTrack(pcm.sampleRate, pcm.channels)
                        trackRate = pcm.sampleRate
                        trackChannels = pcm.channels
                        track.play()
                    }
                    var offset = 0
                    while (offset < pcm.data.size && item.generation == generation.get()) {
                        val written = track.write(pcm.data, offset, minOf(8192, pcm.data.size - offset))
                        if (written < 0) {
                            onError("Audio playback failed (error $written)")
                            break
                        }
                        offset += written
                    }
                    if (item.generation != generation.get()) {
                        track.pause()
                        track.flush()
                        track.play()
                    }
                } finally {
                    if (pending.decrementAndGet() == 0 && item.generation == generation.get()) onDrained()
                }
            }
        } finally {
            track?.release()
        }
    }

    private fun newTrack(rate: Int, channels: Int): AudioTrack {
        val mask = if (channels == 2) AudioFormat.CHANNEL_OUT_STEREO else AudioFormat.CHANNEL_OUT_MONO
        val min = AudioTrack.getMinBufferSize(rate, mask, AudioFormat.ENCODING_PCM_16BIT)
        return AudioTrack.Builder()
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_ASSISTANT)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                    .build(),
            )
            .setAudioFormat(
                AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(rate)
                    .setChannelMask(mask)
                    .build(),
            )
            .setBufferSizeInBytes(maxOf(min, rate / 2))
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
    }
}
