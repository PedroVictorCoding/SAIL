package dev.sail.app.feature.reader

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.sail.app.core.UiState
import dev.sail.app.core.net.ApiResult
import dev.sail.app.feature.library.ReaderBook
import dev.sail.app.feature.library.ReadingAnchor
import dev.sail.app.feature.library.ReadingPosition
import dev.sail.app.feature.library.SectionInfo
import dev.sail.app.feature.library.TocNode
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/** Where the UI must scroll. [toEnd] opens a section at its last page (paging backwards). */
data class ScrollRequest(val anchor: ReadingAnchor, val toEnd: Boolean, val token: Int)

sealed interface SaveStatus {
    data object Idle : SaveStatus
    data object Saved : SaveStatus
    data class Failed(val message: String) : SaveStatus
}

data class ReaderUiState(
    val book: UiState<ReaderBook> = UiState.Loading,
    val sections: List<SectionInfo> = emptyList(),
    val toc: List<TocNode> = emptyList(),
    val section: UiState<ReaderSection> = UiState.Loading,
    val anchor: ReadingAnchor = ReadingAnchor(),
    val percent: Float = 0f,
    val chapter: String? = null,
    val scroll: ScrollRequest? = null,
    val conflict: ReadingPosition? = null,
    val save: SaveStatus = SaveStatus.Idle,
    val prefs: ReaderPrefs = ReaderPrefs(),
    val narration: NarrationUi = NarrationUi(),
) {
    val sectionIndex: Int get() = (section as? UiState.Ready)?.data?.index ?: anchor.section
    val hasPrevious: Boolean get() = sectionIndex > 0
    val hasNext: Boolean get() = sectionIndex < sections.size - 1
}

/** Read-aloud as the screen shows it. */
data class NarrationUi(
    val status: NarrationStatus = NarrationStatus.Off,
    /** The sentence being read (highlighted), in the loaded section. */
    val segment: Segment? = null,
    val index: Int = 0,
    val count: Int = 0,
    /** The page keeps the spoken sentence in view until the owner scrolls away. */
    val follow: Boolean = true,
    /** A sentence the owner long-pressed; nothing moves until they confirm it. */
    val pending: Segment? = null,
    /** Where narration was before the last confirmed jump, for Undo. */
    val undo: Segment? = null,
    val message: String? = null,
    val voices: List<String> = emptyList(),
    val voice: String? = null,
    val speed: Float = 1f,
    val speedRange: SpeedRange = SpeedRange(),
) {
    val active: Boolean get() = status != NarrationStatus.Off
    val playing: Boolean get() = status == NarrationStatus.Playing || status == NarrationStatus.Loading
}

/**
 * Reads one book a section at a time and keeps SAIL's saved position in step with what's on
 * screen. The server is authoritative: saves carry the last revision seen, and a newer save
 * from another device is shown as a conflict for the owner to resolve, never overwritten.
 *
 * [persistScope] outlives this ViewModel so the last save still goes out after leaving.
 */
class ReaderViewModel(
    private val source: ReaderSource,
    private val prefsStore: ReaderPrefsStore,
    private val bookId: String,
    private val startSection: Int?,
    private val persistScope: CoroutineScope,
    private val saveDelayMs: Long = 650,
    /** Builds the audio output; it calls back when another app takes the audio focus. */
    sinkFactory: ((onFocusLost: () -> Unit) -> PcmSink)? = null,
    private val selectionTimeoutMs: Long = 8_000,
    private val undoTimeoutMs: Long = 8_000,
) : ViewModel() {
    private val mutable = MutableStateFlow(ReaderUiState(prefs = prefsStore.load()))
    val state: StateFlow<ReaderUiState> = mutable.asStateFlow()

    private var revision = 0L
    private var dirty = false
    private var change = 0
    private var scrollToken = 0
    private var sectionJob: Job? = null
    private var saveJob: Job? = null
    private val saving = Mutex()

    private var segments: List<Segment> = emptyList()
    private var speech: SpeechInfo? = null
    private var continueNarration = false
    private var selectionJob: Job? = null
    private var undoJob: Job? = null
    private val narrator: Narrator? = sinkFactory?.let { factory ->
        Narrator(
            scope = viewModelScope,
            speaker = { text, voice, speed -> source.speak(text, voice, speed) },
            sink = factory { viewModelScope.launch { pauseNarration() } },
            onStarted = ::segmentStarted,
            onEnded = ::narrationEnded,
        )
    }

    init {
        load()
        narrator?.let { engine ->
            viewModelScope.launch {
                engine.state.collect { narrated ->
                    mutable.update { current ->
                        val n = current.narration
                        current.copy(
                            narration = n.copy(
                                status = narrated.status,
                                index = narrated.index,
                                count = engine.segments.size,
                                segment = if (narrated.status == NarrationStatus.Off) null else engine.segments.getOrNull(narrated.index),
                                message = narrated.error ?: n.message.takeIf { narrated.status == NarrationStatus.Off },
                            ),
                        )
                    }
                }
            }
        }
    }

    // --- Read-aloud -----------------------------------------------------------------------

    /** Start reading aloud from the top of the page, or from segment [from] of this section. */
    fun listen(from: Int? = null) {
        val engine = narrator ?: return
        if (mutable.value.section !is UiState.Ready) return
        viewModelScope.launch {
            val info = speech ?: run {
                narrationUi { it.copy(status = NarrationStatus.Loading, message = null) }
                when (val result = source.speechInfo()) {
                    is ApiResult.Ok -> result.data
                    is ApiResult.Err -> {
                        narrationUi { it.copy(status = NarrationStatus.Off, message = result.message) }
                        return@launch
                    }
                }
            }
            if (!info.available) {
                narrationUi { it.copy(status = NarrationStatus.Off, message = info.reason ?: "The host voice is not running.") }
                return@launch
            }
            speech = info
            val (voice, speed) = voiceAndSpeed(info)
            narrationUi {
                it.copy(follow = true, message = null, voices = info.voices, voice = voice, speed = speed, speedRange = info.speed)
            }
            val start = from ?: segmentIndexAt(segments, mutable.value.anchor)
            engine.load(segments, start, voice, speed, autoplay = true)
        }
    }

    fun togglePlay() {
        val engine = narrator ?: return
        if (mutable.value.narration.playing) engine.pause() else engine.play()
    }

    fun pauseNarration() {
        narrator?.pause()
    }

    fun previousSegment() {
        val engine = narrator ?: return
        if (!engine.active) return
        val index = engine.state.value.index
        if (index > 0) engine.seek(index - 1)
    }

    fun nextSegment() {
        val engine = narrator ?: return
        if (!engine.active) return
        val index = engine.state.value.index
        when {
            index + 1 < engine.segments.size -> engine.seek(index + 1)
            mutable.value.hasNext -> {
                continueNarration = mutable.value.narration.playing
                openSection(ReadingAnchor(mutable.value.sectionIndex + 1, 0, 0), toEnd = false, userMove = false)
            }
        }
    }

    fun stopNarration() {
        narrator?.stop()
        continueNarration = false
        clearSelection()
        undoJob?.cancel()
        narrationUi { it.copy(undo = null, follow = true) }
        flush()
    }

    /** The owner scrolled or turned the page while listening: stop pulling the page along. */
    fun userScrolled() {
        if (mutable.value.narration.active && mutable.value.narration.follow) narrationUi { it.copy(follow = false) }
    }

    fun dismissNarrationMessage() {
        narrationUi { it.copy(message = null) }
    }

    fun followNarration() {
        narrationUi { it.copy(follow = true) }
    }

    fun setVoice(voice: String) {
        updatePrefs { it.copy(voice = voice) }
        applyVoice()
    }

    fun setSpeed(speed: Float) {
        val range = mutable.value.narration.speedRange
        updatePrefs { it.copy(speed = speed.coerceIn(range.min, range.max)) }
        applyVoice()
    }

    /**
     * Step one of choosing a sentence: a long press marks it. Narration keeps going until
     * [confirmSelection]; the mark clears itself after a few seconds.
     */
    fun selectAt(block: Int, char: Int) {
        if (mutable.value.section !is UiState.Ready || narrator == null) return
        val index = segmentAt(segments, block, char) ?: return
        narrationUi { it.copy(pending = segments[index]) }
        selectionJob?.cancel()
        selectionJob = viewModelScope.launch {
            delay(selectionTimeoutMs)
            clearSelection()
        }
    }

    fun clearSelection() {
        selectionJob?.cancel()
        narrationUi { it.copy(pending = null) }
    }

    /** Step two: read from the marked sentence. The previous place stays available as Undo. */
    fun confirmSelection() {
        val engine = narrator ?: return
        val target = mutable.value.narration.pending ?: return
        val index = segments.indexOf(target).takeIf { it >= 0 } ?: return clearSelection()
        clearSelection()
        if (!engine.active || mutable.value.narration.status == NarrationStatus.Error) {
            listen(from = index)
            return
        }
        val before = engine.segments.getOrNull(engine.state.value.index)
        if (before != null && before != target) {
            narrationUi { it.copy(undo = before) }
            undoJob?.cancel()
            undoJob = viewModelScope.launch {
                delay(undoTimeoutMs)
                narrationUi { it.copy(undo = null) }
            }
        }
        narrationUi { it.copy(follow = true) }
        engine.seek(index)
        engine.play()
    }

    fun undoJump() {
        val engine = narrator ?: return
        val back = mutable.value.narration.undo ?: return
        undoJob?.cancel()
        narrationUi { it.copy(undo = null, follow = true) }
        val index = engine.segments.indexOf(back)
        if (index >= 0) {
            engine.seek(index)
            engine.play()
        }
    }

    private fun applyVoice() {
        val info = speech ?: return
        val (voice, speed) = voiceAndSpeed(info)
        narrationUi { it.copy(voice = voice, speed = speed) }
        narrator?.configure(voice, speed)
    }

    private fun voiceAndSpeed(info: SpeechInfo): Pair<String, Float> {
        val prefs = mutable.value.prefs
        val voice = prefs.voice?.takeIf { it in info.voices }
            ?: info.defaultVoice.takeIf { info.voices.isEmpty() || it in info.voices }
            ?: info.voices.first()
        return voice to prefs.speed.coerceIn(info.speed.min, info.speed.max)
    }

    private fun segmentStarted(index: Int) {
        val segment = narrator?.segments?.getOrNull(index) ?: return
        if (segment.anchor.section != mutable.value.sectionIndex) return
        // The spoken sentence is the reading position while listening.
        setAnchor(segment.anchor, scroll = mutable.value.scroll)
        markDirty()
    }

    private fun narrationEnded() {
        if (mutable.value.hasNext) {
            continueNarration = true
            openSection(ReadingAnchor(mutable.value.sectionIndex + 1, 0, 0), toEnd = false, userMove = false)
        } else {
            narrator?.stop()
            narrationUi { it.copy(message = "End of the book") }
        }
    }

    private inline fun narrationUi(crossinline transform: (NarrationUi) -> NarrationUi) {
        mutable.update { it.copy(narration = transform(it.narration)) }
    }

    // --- Reading ----------------------------------------------------------------------------

    fun load() {
        mutable.update { it.copy(book = UiState.Loading) }
        viewModelScope.launch {
            when (val result = source.book(bookId)) {
                is ApiResult.Err -> mutable.update { it.copy(book = UiState.Unavailable(result.message)) }
                is ApiResult.Ok -> {
                    val detail = result.data
                    if (detail.sections.isEmpty()) {
                        mutable.update {
                            it.copy(book = UiState.Empty("This book has no text", "SAIL found no sections to read."))
                        }
                        return@launch
                    }
                    revision = detail.book.position?.revision ?: 0
                    mutable.update {
                        it.copy(book = UiState.Ready(detail.book), sections = detail.sections, toc = detail.toc)
                    }
                    val start = startSection?.let { ReadingAnchor(it, 0, 0) }
                        ?: detail.book.position?.anchor
                        ?: ReadingAnchor()
                    // Opening at a chosen section is a move the owner made, so it's saved.
                    openSection(start, toEnd = false, userMove = startSection != null)
                }
            }
        }
    }

    /** Reload the current section after a failed load. */
    fun retrySection() {
        openSection(mutable.value.anchor, toEnd = false, userMove = false)
    }

    fun nextSection() {
        val current = mutable.value
        if (current.hasNext) openSection(ReadingAnchor(current.sectionIndex + 1, 0, 0), toEnd = false, userMove = true)
    }

    fun previousSection(atEnd: Boolean = true) {
        val current = mutable.value
        if (current.hasPrevious) openSection(ReadingAnchor(current.sectionIndex - 1, 0, 0), toEnd = atEnd, userMove = true)
    }

    /** Jump from the contents sheet. */
    fun jumpTo(anchor: ReadingAnchor) {
        openSection(anchor, toEnd = false, userMove = true)
    }

    /** The UI finished a [ScrollRequest]; [anchor] is where the page actually landed. */
    fun scrolled(token: Int, anchor: ReadingAnchor) {
        val request = mutable.value.scroll ?: return
        if (request.token != token) return
        setAnchor(anchor, scroll = null)
        if (pendingUserMove) markDirty()
        pendingUserMove = false
    }

    private var pendingUserMove = false

    /** The top of the page moved while reading. Ignored while a jump is still scrolling. */
    fun visible(anchor: ReadingAnchor) {
        val current = mutable.value
        if (current.scroll != null || current.conflict != null) return
        // While listening, the place is the spoken sentence, not wherever the owner looks.
        if (current.narration.active) return
        if (current.section !is UiState.Ready || anchor.section != current.sectionIndex) return
        if (anchor == current.anchor) return
        setAnchor(anchor, scroll = null)
        markDirty()
    }

    /** Save now, e.g. when the app goes to the background or the reader closes. */
    fun flush() {
        saveJob?.cancel()
        if (dirty) persistScope.launch { save() }
    }

    /** Conflict: this phone's place wins and overwrites the other device's. */
    fun keepHere() {
        val remote = mutable.value.conflict ?: return
        revision = remote.revision
        mutable.update { it.copy(conflict = null) }
        dirty = true
        change++
        persistScope.launch { save() }
    }

    /** Conflict: go to the place saved on the other device. */
    fun goThere() {
        val remote = mutable.value.conflict ?: return
        revision = remote.revision
        dirty = false
        mutable.update { it.copy(conflict = null) }
        openSection(remote.anchor, toEnd = false, userMove = false)
    }

    fun updatePrefs(transform: (ReaderPrefs) -> ReaderPrefs) {
        val next = transform(mutable.value.prefs).let {
            it.copy(textSize = it.textSize.coerceIn(ReaderPrefs.MIN_TEXT_SIZE, ReaderPrefs.MAX_TEXT_SIZE))
        }
        val previous = mutable.value.prefs
        mutable.update { it.copy(prefs = next) }
        prefsStore.save(next)
        // New text size or typeface reflows the page: keep the same place at the top.
        val reflowed = next.textSize != previous.textSize || next.serif != previous.serif
        if (reflowed && mutable.value.section is UiState.Ready && mutable.value.scroll == null) {
            pendingUserMove = false
            scrollToken++
            mutable.update { it.copy(scroll = ScrollRequest(it.anchor, toEnd = false, token = scrollToken)) }
        }
    }

    override fun onCleared() {
        narrator?.stop()
        flush()
    }

    private fun openSection(target: ReadingAnchor, toEnd: Boolean, userMove: Boolean) {
        val sections = mutable.value.sections
        if (sections.isEmpty()) return
        val index = target.section.coerceIn(0, sections.size - 1)
        sectionJob?.cancel()
        mutable.update { it.copy(section = UiState.Loading, scroll = null) }
        sectionJob = viewModelScope.launch {
            when (val result = source.section(bookId, index)) {
                is ApiResult.Err -> mutable.update { it.copy(section = UiState.Unavailable(result.message)) }
                is ApiResult.Ok -> {
                    val section = result.data.copy(index = index)
                    val anchor = if (toEnd) {
                        ReadingAnchor(index, (section.blocks.size - 1).coerceAtLeast(0), 0)
                    } else {
                        clampAnchor(target.copy(section = index), sections.size, section.blocks.size)
                    }
                    pendingUserMove = userMove
                    scrollToken++
                    segments = segmentSection(section.blocks, index)
                    selectionJob?.cancel()
                    undoJob?.cancel()
                    mutable.update {
                        it.copy(section = UiState.Ready(section), narration = it.narration.copy(pending = null, undo = null))
                    }
                    setAnchor(anchor, scroll = ScrollRequest(anchor, toEnd, scrollToken))
                    // Narration follows the reader into the new section (or carries on into it).
                    val engine = narrator
                    val info = speech
                    if (engine != null && info != null && (engine.active || continueNarration)) {
                        val autoplay = continueNarration || mutable.value.narration.playing
                        continueNarration = false
                        val (voice, speed) = voiceAndSpeed(info)
                        engine.load(segments, segmentIndexAt(segments, anchor), voice, speed, autoplay)
                        narrationUi { it.copy(follow = true) }
                    }
                }
            }
        }
    }

    private fun setAnchor(anchor: ReadingAnchor, scroll: ScrollRequest?) {
        mutable.update { current ->
            val book = (current.book as? UiState.Ready)?.data ?: return@update current
            val section = (current.section as? UiState.Ready)?.data
            current.copy(
                anchor = anchor,
                percent = percentAt(anchor, book, current.sections, section),
                chapter = chapterAt(current.toc, anchor) ?: section?.title,
                scroll = scroll,
            )
        }
    }

    private fun markDirty() {
        dirty = true
        change++
        saveJob?.cancel()
        // Only the wait is cancellable; a save that has started always finishes, so a PUT the
        // server already applied is never retried with a stale revision.
        saveJob = viewModelScope.launch {
            delay(saveDelayMs)
            persistScope.launch { save() }
        }
    }

    private suspend fun save() = saving.withLock {
        val current = mutable.value
        if (!dirty || current.conflict != null || current.book !is UiState.Ready) return@withLock
        val sent = change
        val anchor = current.anchor
        when (val outcome = source.savePosition(bookId, anchor, current.percent, revision)) {
            is SaveOutcome.Saved -> {
                revision = outcome.position.revision
                if (change == sent) dirty = false
                mutable.update { it.copy(save = SaveStatus.Saved) }
            }
            is SaveOutcome.Conflict -> mutable.update { it.copy(conflict = outcome.remote) }
            is SaveOutcome.Failed -> mutable.update { it.copy(save = SaveStatus.Failed(outcome.message)) }
        }
    }
}
