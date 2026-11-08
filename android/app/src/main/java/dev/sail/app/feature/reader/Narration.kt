package dev.sail.app.feature.reader

import dev.sail.app.feature.library.ReadingAnchor
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * One spoken unit: a sentence, or a slice of a long one. [anchor] is where it starts
 * (UTF-16 offset), [end] where it stops in the same block.
 */
data class Segment(val text: String, val anchor: ReadingAnchor, val end: Int) {
    val block: Int get() = anchor.block
    val start: Int get() = anchor.char
}

/** Sentence budget in characters; the server takes at most 600 per request. */
const val SEGMENT_BUDGET = 320

private val SENTENCE_END = Regex("""[.!?。！？](?:["'”’])?(?:\s|$)""")

/**
 * Cuts a section into sentences, the same way the web reader does (`SailReaderAudio.segment`):
 * a sentence ends at . ! ? (optionally closed by a quote) followed by space; anything longer
 * than [budget] is cut at the last space past half the budget. Surrogate pairs are never split.
 */
fun segmentSection(blocks: List<ReaderBlock>, section: Int, budget: Int = SEGMENT_BUDGET): List<Segment> {
    val size = budget.coerceIn(2, 1000)
    val out = mutableListOf<Segment>()
    blocks.forEachIndexed { b, block ->
        val text = block.text
        var start = 0
        while (start < text.length) {
            val limit = safeCut(text, minOf(start + size, text.length))
            val part = text.substring(start, limit)
            val sentence = SENTENCE_END.find(part)
            var end = if (sentence != null) start + sentence.range.last + 1 else limit
            if (sentence == null && limit < text.length) {
                val space = part.lastIndexOf(' ')
                if (space > size / 2) end = start + space + 1
            }
            if (end <= start) end = minOf(text.length, start + 1)
            if (text.substring(start, end).isNotBlank()) {
                out += Segment(text.substring(start, end), ReadingAnchor(section, b, start), end)
            }
            start = end
        }
    }
    return out
}

private fun safeCut(text: String, offset: Int): Int =
    if (offset > 0 && offset < text.length && text[offset].isLowSurrogate() && text[offset - 1].isHighSurrogate()) offset - 1 else offset

/** The segment holding [anchor], or the first one after it; `segments.size` past the end. */
fun segmentIndexAt(segments: List<Segment>, anchor: ReadingAnchor): Int {
    val index = segments.indexOfFirst { it.block > anchor.block || (it.block == anchor.block && it.end > anchor.char) }
    return if (index < 0) segments.size else index
}

/** The segment under a character the owner pressed, or null if they pressed a gap. */
fun segmentAt(segments: List<Segment>, block: Int, char: Int): Int? =
    segments.indexOfFirst { it.block == block && char >= it.start && char < it.end }.takeIf { it >= 0 }
        ?: segments.indexOfLast { it.block == block && char >= it.start }.takeIf { it >= 0 }

/** `GET /api/reader/speech`. */
@Serializable
data class SpeechInfo(
    val available: Boolean = false,
    val voices: List<String> = emptyList(),
    @SerialName("default_voice") val defaultVoice: String = "af_heart",
    val speed: SpeedRange = SpeedRange(),
    @SerialName("max_chars") val maxChars: Int = 600,
    val reason: String? = null,
)

@Serializable
data class SpeedRange(val min: Float = 0.7f, val max: Float = 1.5f, val default: Float = 1f)

@Serializable
data class SpeechRequest(val text: String, val voice: String, val speed: Float)

/** Mono 16-bit little-endian PCM. */
class Pcm(val bytes: ByteArray, val sampleRate: Int) {
    val durationMs: Long get() = if (sampleRate <= 0) 0 else bytes.size / 2 * 1000L / sampleRate
}

sealed interface SpeechOutcome {
    class Audio(val pcm: Pcm) : SpeechOutcome

    /** The host voice is reading another sentence (HTTP 429); try again shortly. */
    data object Busy : SpeechOutcome

    data class Failed(val message: String) : SpeechOutcome
}

fun interface Speaker {
    suspend fun speak(text: String, voice: String, speed: Float): SpeechOutcome
}

/** Plays PCM. [play] returns once the audio has been heard (or throws on cancellation). */
interface PcmSink {
    suspend fun play(pcm: Pcm)

    fun pause()

    fun resume()

    /** Drop what's playing now (seek, new text). */
    fun stop()

    /** Narration is over: free the output and its audio focus. Playing again reopens it. */
    fun release() {}
}

enum class NarrationStatus { Off, Loading, Playing, Paused, Error }

data class NarratorState(
    val status: NarrationStatus = NarrationStatus.Off,
    /** The segment playing, about to play, or paused on. */
    val index: Int = 0,
    val error: String? = null,
)

/**
 * Speaks a list of segments in order, fetching up to [ahead] segments in advance. Requests
 * are serial (the host voice serves one at a time). [onStarted] fires as each segment begins
 * to sound, [onEnded] after the last one.
 */
class Narrator(
    private val scope: CoroutineScope,
    private val speaker: Speaker,
    private val sink: PcmSink,
    private val onStarted: (Int) -> Unit,
    private val onEnded: () -> Unit,
    private val ahead: Int = 2,
    private val busyRetryMs: Long = 300,
) {
    private val mutable = MutableStateFlow(NarratorState())
    val state: StateFlow<NarratorState> = mutable.asStateFlow()

    var segments: List<Segment> = emptyList()
        private set
    private var voice = ""
    private var speed = 1f
    private val cache = HashMap<Int, Deferred<SpeechOutcome>>()
    private val requests = Mutex()
    private var loop: Job? = null

    val active: Boolean get() = mutable.value.status != NarrationStatus.Off && mutable.value.status != NarrationStatus.Error

    /** Replace the text (a new section) and start at [index], playing if [autoplay]. */
    fun load(segments: List<Segment>, index: Int, voice: String, speed: Float, autoplay: Boolean) {
        halt()
        clearCache()
        this.segments = segments
        this.voice = voice
        this.speed = speed
        val start = index.coerceIn(0, segments.size)
        mutable.value = NarratorState(if (autoplay) NarrationStatus.Loading else NarrationStatus.Paused, start)
        if (autoplay) run(start)
    }

    fun play() {
        when (mutable.value.status) {
            NarrationStatus.Paused -> {
                if (loop?.isActive == true) {
                    mutable.update { it.copy(status = NarrationStatus.Playing) }
                    sink.resume()
                } else {
                    run(mutable.value.index)
                }
            }
            NarrationStatus.Error -> run(mutable.value.index)
            else -> Unit
        }
    }

    fun pause() {
        val status = mutable.value.status
        if (status == NarrationStatus.Playing || status == NarrationStatus.Loading) {
            mutable.update { it.copy(status = NarrationStatus.Paused) }
            sink.pause()
        }
    }

    /** Go to segment [index], keeping play or pause as it was. */
    fun seek(index: Int) {
        val target = index.coerceIn(0, (segments.size - 1).coerceAtLeast(0))
        val playing = mutable.value.status.let { it == NarrationStatus.Playing || it == NarrationStatus.Loading }
        halt()
        keepOnly(target)
        if (playing) {
            mutable.value = NarratorState(NarrationStatus.Loading, target)
            run(target)
        } else {
            mutable.value = NarratorState(NarrationStatus.Paused, target)
        }
    }

    /** New voice or speed: audio already fetched no longer matches, so the segment restarts. */
    fun configure(voice: String, speed: Float) {
        if (voice == this.voice && speed == this.speed) return
        this.voice = voice
        this.speed = speed
        if (!active) return
        val index = mutable.value.index
        val playing = mutable.value.status != NarrationStatus.Paused
        halt()
        clearCache()
        mutable.value = NarratorState(if (playing) NarrationStatus.Loading else NarrationStatus.Paused, index)
        if (playing) run(index)
    }

    fun stop() {
        halt()
        clearCache()
        sink.release()
        mutable.value = NarratorState()
    }

    private fun halt() {
        loop?.cancel()
        loop = null
        sink.stop()
    }

    private fun run(from: Int) {
        loop = scope.launch {
            var i = from
            while (i < segments.size) {
                for (j in i..minOf(segments.size - 1, i + ahead)) fetch(j)
                if (mutable.value.status != NarrationStatus.Paused) {
                    mutable.update { it.copy(status = NarrationStatus.Loading, index = i, error = null) }
                } else {
                    mutable.update { it.copy(index = i) }
                }
                when (val outcome = fetch(i).await()) {
                    is SpeechOutcome.Failed -> {
                        // Prefetched sentences likely failed the same way; retry fetches afresh.
                        clearCache()
                        mutable.value = NarratorState(NarrationStatus.Error, i, outcome.message)
                        return@launch
                    }
                    SpeechOutcome.Busy -> error("busy is retried in fetch")
                    is SpeechOutcome.Audio -> {
                        // A pause during loading holds the segment until play.
                        mutable.first { it.status != NarrationStatus.Paused }
                        mutable.update { it.copy(status = NarrationStatus.Playing, index = i) }
                        onStarted(i)
                        sink.play(outcome.pcm)
                        cache.remove(i)
                    }
                }
                i++
            }
            mutable.update { it.copy(index = segments.size) }
            onEnded()
        }
    }

    private fun fetch(index: Int): Deferred<SpeechOutcome> {
        cache[index]?.takeIf { !it.isCancelled }?.let { return it }
        val text = segments[index].text
        val voice = voice
        val speed = speed
        return scope.async {
            requests.withLock {
                var outcome = speaker.speak(text, voice, speed)
                while (outcome == SpeechOutcome.Busy) {
                    delay(busyRetryMs)
                    outcome = speaker.speak(text, voice, speed)
                }
                outcome
            }
        }.also { cache[index] = it }
    }

    private fun keepOnly(from: Int) {
        val keep = from..from + ahead
        cache.keys.filter { it !in keep }.forEach { cache.remove(it)?.cancel() }
    }

    private fun clearCache() {
        cache.values.forEach { it.cancel() }
        cache.clear()
    }
}
