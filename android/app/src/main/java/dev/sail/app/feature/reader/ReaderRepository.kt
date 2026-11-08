package dev.sail.app.feature.reader

import android.content.Context
import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.BinaryBody
import dev.sail.app.core.net.SailApi
import dev.sail.app.feature.library.ReaderBookDetail
import dev.sail.app.feature.library.ReadingAnchor

interface ReaderSource {
    suspend fun book(id: String): ApiResult<ReaderBookDetail>

    suspend fun section(id: String, index: Int): ApiResult<ReaderSection>

    suspend fun savePosition(id: String, anchor: ReadingAnchor, percent: Float, expectedRevision: Long): SaveOutcome

    suspend fun speechInfo(): ApiResult<SpeechInfo> = ApiResult.Err(404, "Read-aloud isn't available")

    suspend fun speak(text: String, voice: String, speed: Float): SpeechOutcome =
        SpeechOutcome.Failed("Read-aloud isn't available")
}

class ReaderRepository(private val api: SailApi) : ReaderSource {
    override suspend fun book(id: String): ApiResult<ReaderBookDetail> =
        api.get<ReaderBookDetail>("/api/reader/books/$id")

    override suspend fun section(id: String, index: Int): ApiResult<ReaderSection> =
        api.get<ReaderSection>("/api/reader/books/$id/sections/$index")

    override suspend fun savePosition(
        id: String,
        anchor: ReadingAnchor,
        percent: Float,
        expectedRevision: Long,
    ): SaveOutcome {
        val path = "/api/reader/books/$id/position"
        return when (val result = api.putJson<PositionRequest, PositionResponse>(path, PositionRequest(anchor, percent, expectedRevision))) {
            is ApiResult.Ok -> result.data.position?.let { SaveOutcome.Saved(it) }
                ?: SaveOutcome.Failed("SAIL didn't confirm the saved position")
            is ApiResult.Err -> if (result.status == 409) {
                // Another device saved since this one last did; ask SAIL where that was.
                when (val remote = api.get<PositionResponse>(path)) {
                    is ApiResult.Ok -> remote.data.position?.let { SaveOutcome.Conflict(it) }
                        ?: SaveOutcome.Failed(result.message)
                    is ApiResult.Err -> SaveOutcome.Failed(result.message)
                }
            } else {
                SaveOutcome.Failed(result.message)
            }
        }
    }

    override suspend fun speechInfo(): ApiResult<SpeechInfo> = api.get<SpeechInfo>("/api/reader/speech")

    /** The text is spoken literally by the host's voice; nothing reaches a model. */
    override suspend fun speak(text: String, voice: String, speed: Float): SpeechOutcome =
        speechOutcome(api.postJsonForBytes("/api/reader/speech", SpeechRequest(text, voice, speed)))
}

/** Reads `/api/reader/speech`'s answer: raw PCM with its rate in `X-Sample-Rate`. */
fun speechOutcome(result: ApiResult<BinaryBody>): SpeechOutcome = when (result) {
    is ApiResult.Ok -> {
        val rate = result.data.headers["x-sample-rate"]?.toIntOrNull() ?: 0
        val bytes = result.data.bytes
        if (rate <= 0 || bytes.isEmpty() || bytes.size % 2 != 0) {
            SpeechOutcome.Failed("SAIL sent audio this app can't play")
        } else {
            SpeechOutcome.Audio(Pcm(bytes, rate))
        }
    }
    is ApiResult.Err -> if (result.status == 429) SpeechOutcome.Busy else SpeechOutcome.Failed(result.message)
}

/** Reading preferences are per phone, like the web reader's local display settings. */
interface ReaderPrefsStore {
    fun load(): ReaderPrefs

    fun save(prefs: ReaderPrefs)
}

class SharedReaderPrefs(context: Context) : ReaderPrefsStore {
    private val prefs = context.getSharedPreferences("sail_reader", Context.MODE_PRIVATE)

    override fun load(): ReaderPrefs = ReaderPrefs(
        theme = ReaderTheme.entries.firstOrNull { it.name == prefs.getString("theme", null) } ?: ReaderTheme.Night,
        textSize = prefs.getInt("text_size", ReaderPrefs.DEFAULT_TEXT_SIZE)
            .coerceIn(ReaderPrefs.MIN_TEXT_SIZE, ReaderPrefs.MAX_TEXT_SIZE),
        serif = prefs.getBoolean("serif", true),
        keepScreenOn = prefs.getBoolean("keep_screen_on", true),
        voice = prefs.getString("voice", null),
        speed = prefs.getFloat("speed", 1f),
    )

    override fun save(prefs: ReaderPrefs) {
        this.prefs.edit()
            .putString("theme", prefs.theme.name)
            .putInt("text_size", prefs.textSize)
            .putBoolean("serif", prefs.serif)
            .putBoolean("keep_screen_on", prefs.keepScreenOn)
            .putString("voice", prefs.voice)
            .putFloat("speed", prefs.speed)
            .apply()
    }
}
