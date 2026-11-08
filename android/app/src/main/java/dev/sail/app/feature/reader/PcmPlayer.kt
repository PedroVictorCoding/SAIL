package dev.sail.app.feature.reader

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext

/**
 * Plays narration PCM on one streaming AudioTrack. [play] returns when the track's playback
 * head has passed the segment's last frame, so the next highlight starts when its audio does.
 * Holds audio focus while narrating; [onFocusLost] lets the reader pause for a call or another
 * app's audio.
 */
class PcmPlayer(context: Context, private val onFocusLost: () -> Unit) : PcmSink {
    private val audio = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
    private val attributes = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_MEDIA)
        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
        .build()
    private val focus = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
        .setAudioAttributes(attributes)
        .setOnAudioFocusChangeListener { change ->
            if (change == AudioManager.AUDIOFOCUS_LOSS || change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT) onFocusLost()
        }
        .build()

    private val lock = Any()
    private var track: AudioTrack? = null
    private var rate = 0

    /** Frames written since the track was last flushed; the head position counts the same way. */
    private var written = 0L
    private var hasFocus = false

    override suspend fun play(pcm: Pcm) = withContext(Dispatchers.IO) {
        if (pcm.bytes.isEmpty() || pcm.sampleRate <= 0) return@withContext
        val current = synchronized(lock) {
            if (!hasFocus) hasFocus = audio.requestAudioFocus(focus) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
            val existing = track
            val usable = if (existing != null && rate == pcm.sampleRate) {
                existing
            } else {
                existing?.release()
                newTrack(pcm.sampleRate).also {
                    track = it
                    rate = pcm.sampleRate
                    written = 0
                }
            }
            if (usable.playState != AudioTrack.PLAYSTATE_PLAYING && usable.playState != AudioTrack.PLAYSTATE_PAUSED) usable.play()
            usable
        }
        var offset = 0
        while (offset < pcm.bytes.size) {
            ensureActive()
            val count = current.write(pcm.bytes, offset, minOf(8192, pcm.bytes.size - offset))
            if (count < 0) error("Audio playback failed (error $count)")
            if (count == 0) delay(10)
            offset += count
        }
        val target = synchronized(lock) {
            written += pcm.bytes.size / 2
            written
        }
        // Wait until it has been heard. The head is an unsigned 32-bit frame count.
        while (true) {
            ensureActive()
            val head = current.playbackHeadPosition.toLong() and 0xFFFFFFFFL
            if (head >= target) break
            delay(20)
        }
    }

    override fun pause() {
        synchronized(lock) { track?.pause() }
    }

    override fun resume() {
        synchronized(lock) { track?.play() }
    }

    override fun stop() {
        synchronized(lock) {
            track?.let {
                // Pause then flush drops what's buffered and resets the head; this also
                // releases a write blocked on a full buffer.
                it.pause()
                it.flush()
            }
            written = 0
        }
    }

    /** Narration ended: give the audio focus back and free the track. */
    override fun release() {
        stop()
        synchronized(lock) {
            track?.release()
            track = null
            if (hasFocus) {
                audio.abandonAudioFocusRequest(focus)
                hasFocus = false
            }
        }
    }

    private fun newTrack(sampleRate: Int): AudioTrack {
        val mask = AudioFormat.CHANNEL_OUT_MONO
        val min = AudioTrack.getMinBufferSize(sampleRate, mask, AudioFormat.ENCODING_PCM_16BIT)
        return AudioTrack.Builder()
            .setAudioAttributes(attributes)
            .setAudioFormat(
                AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(sampleRate)
                    .setChannelMask(mask)
                    .build(),
            )
            .setBufferSizeInBytes(maxOf(min, sampleRate / 2))
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
    }
}
