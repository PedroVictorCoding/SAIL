package dev.sail.app.feature.reader

import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.animateScrollBy
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.sail.app.core.UiState
import dev.sail.app.core.design.EmptyState
import dev.sail.app.core.design.LoadingSkeleton
import dev.sail.app.core.design.PrimaryButton
import dev.sail.app.core.design.ScreenGutter
import dev.sail.app.core.design.SecondaryButton
import dev.sail.app.core.design.animationsEnabled
import dev.sail.app.core.sailViewModel
import dev.sail.app.feature.library.ReadingAnchor
import dev.sail.app.feature.library.percentText
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.launch

/** Reading column width on wide screens; phones use the full width minus the gutter. */
private val MaxColumn = 640.dp

/**
 * The e-reader: one section at a time, laid out by block kind. Tap the left or right third to
 * turn a page (it continues into the previous or next section at the edges), the middle to show
 * or hide the controls. The position is saved to SAIL as the page moves.
 */
@Composable
fun ReaderScreen(bookId: String, startSection: Int?, onBack: () -> Unit) {
    val app = LocalContext.current.applicationContext
    val vm = sailViewModel(key = "reader:$bookId:${startSection ?: "resume"}") {
        ReaderViewModel(ReaderRepository(it.api), SharedReaderPrefs(app), bookId, startSection, it.appScope)
    }
    val state by vm.state.collectAsStateWithLifecycle()
    val theme = state.prefs.theme
    val colors = remember(theme) { ReaderColors(theme) }

    // Save when the app leaves the foreground, not only when the reader closes.
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    DisposableEffect(lifecycle) {
        val observer = LifecycleEventObserver { _, event -> if (event == Lifecycle.Event.ON_STOP) vm.flush() }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }
    val view = LocalView.current
    DisposableEffect(state.prefs.keepScreenOn) {
        view.keepScreenOn = state.prefs.keepScreenOn
        onDispose { view.keepScreenOn = false }
    }

    var chrome by rememberSaveable { mutableStateOf(true) }
    var sheet by rememberSaveable { mutableStateOf(ReaderSheet.None) }
    val close = {
        vm.flush()
        onBack()
    }
    BackHandler(onBack = close)

    Box(
        Modifier
            .fillMaxSize()
            .background(colors.background),
    ) {
        when (val book = state.book) {
            UiState.Loading -> LoadingSkeleton(lines = 10, modifier = Modifier.padding(ScreenGutter).padding(top = 56.dp))
            is UiState.Unavailable -> Column(Modifier.fillMaxSize()) {
                ReaderTopBar(colors, title = "Book", subtitle = null, onBack = close, actions = false, onContents = {}, onText = {})
                EmptyState(
                    title = "This book couldn't open",
                    detail = book.message,
                    onRetry = vm::load,
                    modifier = Modifier.padding(ScreenGutter),
                )
            }
            is UiState.Empty -> Column(Modifier.fillMaxSize()) {
                ReaderTopBar(colors, title = "Book", subtitle = null, onBack = close, actions = false, onContents = {}, onText = {})
                EmptyState(title = book.title, detail = book.detail, modifier = Modifier.padding(ScreenGutter))
            }
            is UiState.Ready -> {
                when (val section = state.section) {
                    UiState.Loading -> LoadingSkeleton(lines = 10, modifier = Modifier.padding(ScreenGutter).padding(top = 56.dp))
                    is UiState.Unavailable -> EmptyState(
                        title = "This section couldn't load",
                        detail = section.message,
                        onRetry = vm::retrySection,
                        modifier = Modifier.padding(ScreenGutter).padding(top = 64.dp),
                    )
                    is UiState.Empty -> EmptyState(title = section.title, detail = section.detail, modifier = Modifier.padding(ScreenGutter).padding(top = 64.dp))
                    is UiState.Ready -> key(section.data.index) {
                        SectionPages(
                            section = section.data,
                            state = state,
                            colors = colors,
                            vm = vm,
                            onToggleChrome = { chrome = !chrome },
                            onPageTurned = { chrome = false },
                        )
                    }
                }
                AnimatedVisibility(
                    visible = chrome,
                    enter = fadeIn(),
                    exit = fadeOut(),
                    modifier = Modifier.align(Alignment.TopCenter),
                ) {
                    ReaderTopBar(
                        colors,
                        title = book.data.displayTitle,
                        subtitle = state.chapter,
                        onBack = close,
                        actions = true,
                        onContents = { sheet = ReaderSheet.Contents },
                        onText = { sheet = ReaderSheet.Text },
                    )
                }
                AnimatedVisibility(
                    visible = chrome,
                    enter = fadeIn(),
                    exit = fadeOut(),
                    modifier = Modifier.align(Alignment.BottomCenter),
                ) {
                    ReaderBottomBar(state, colors, onPrevious = { vm.previousSection(atEnd = false) }, onNext = vm::nextSection)
                }
                if (!chrome) {
                    StatusLine(state, colors, Modifier.align(Alignment.BottomCenter))
                }
            }
        }

        state.conflict?.let { remote ->
            ConflictCard(
                remotePercent = remote.percent,
                colors = colors,
                onKeepHere = vm::keepHere,
                onGoThere = vm::goThere,
                modifier = Modifier.align(Alignment.Center),
            )
        }
    }

    when (sheet) {
        ReaderSheet.None -> Unit
        ReaderSheet.Contents -> ContentsSheet(
            entries = remember(state.toc, state.sections) { flattenToc(state.toc, state.sections) },
            current = state.anchor,
            colors = colors,
            onPick = {
                sheet = ReaderSheet.None
                chrome = false
                vm.jumpTo(it)
            },
            onDismiss = { sheet = ReaderSheet.None },
        )
        ReaderSheet.Text -> TextSheet(
            prefs = state.prefs,
            colors = colors,
            onChange = vm::updatePrefs,
            onDismiss = { sheet = ReaderSheet.None },
        )
    }
}

private enum class ReaderSheet { None, Contents, Text }

private class ReaderColors(theme: ReaderTheme) {
    val background = Color(theme.background)
    val text = Color(theme.text)
    val muted = Color(theme.muted)
    val accent = Color(theme.accent)
    val chrome = Color(theme.background).copy(alpha = 0.97f)
    val line = Color(theme.text).copy(alpha = 0.12f)
}

@Composable
private fun SectionPages(
    section: ReaderSection,
    state: ReaderUiState,
    colors: ReaderColors,
    vm: ReaderViewModel,
    onToggleChrome: () -> Unit,
    onPageTurned: () -> Unit,
) {
    val prefs = state.prefs
    val listState = rememberLazyListState()
    val density = LocalDensity.current
    val scope = rememberCoroutineScope()
    val animate = animationsEnabled()
    // Text layouts by block, to turn the scroll position into a character offset and back.
    val layouts = remember(prefs.textSize, prefs.serif) { HashMap<Int, TextLayoutResult>() }
    val family = if (prefs.serif) FontFamily.Serif else FontFamily.SansSerif
    val lineHeightPx = with(density) { (prefs.textSize * 1.6f).sp.toPx() }
    val gaps = remember(section.index, prefs.textSize) { section.blocks.map { topGap(it, prefs.textSize) } }
    val gapPx = { block: Int -> with(density) { gaps.getOrElse(block) { 0.dp }.toPx() } }

    fun anchorNow(): ReadingAnchor = anchorAt(
        section = section,
        firstIndex = listState.firstVisibleItemIndex,
        firstOffset = listState.firstVisibleItemScrollOffset.toFloat(),
        layouts = layouts,
        gapPx = gapPx,
    )

    // Carry out jumps the ViewModel asks for, then tell it where the page landed.
    val request = state.scroll
    LaunchedEffect(request?.token) {
        val target = request ?: return@LaunchedEffect
        if (target.toEnd) {
            listState.scrollToItem(section.blocks.size)
        } else {
            val block = target.anchor.block.coerceIn(0, (section.blocks.size - 1).coerceAtLeast(0))
            listState.scrollToItem(block)
            if (target.anchor.char > 0) {
                withFrameNanos { }
                layouts[block]?.let { layout ->
                    val char = target.anchor.char.coerceIn(0, layout.layoutInput.text.length)
                    val line = layout.getLineForOffset(char)
                    listState.scrollToItem(block, (gapPx(block) + layout.getLineTop(line)).toInt())
                }
            }
        }
        withFrameNanos { }
        vm.scrolled(target.token, anchorNow())
    }

    // Report the place whenever a scroll or page turn settles.
    LaunchedEffect(listState) {
        snapshotFlow { listState.isScrollInProgress }
            .distinctUntilChanged()
            .filter { !it }
            .collect { vm.visible(anchorNow()) }
    }

    fun turn(forward: Boolean) {
        onPageTurned()
        scope.launch {
            val viewport = listState.layoutInfo.viewportSize.height.toFloat()
            // Keep one line of overlap so the reader doesn't lose their place.
            val step = (viewport - lineHeightPx * 1.2f).coerceAtLeast(lineHeightPx)
            when {
                forward && !listState.canScrollForward -> vm.nextSection()
                !forward && !listState.canScrollBackward -> vm.previousSection(atEnd = true)
                animate -> listState.animateScrollBy(if (forward) step else -step)
                else -> listState.scrollBy(if (forward) step else -step)
            }
        }
    }

    val currentTurn by rememberUpdatedState(::turn)
    val currentToggle by rememberUpdatedState(onToggleChrome)
    Box(
        Modifier
            .fillMaxSize()
            .pointerInputTaps(onLeft = { currentTurn(false) }, onRight = { currentTurn(true) }, onMiddle = { currentToggle() }),
        contentAlignment = Alignment.TopCenter,
    ) {
        LazyColumn(
            state = listState,
            modifier = Modifier
                .widthIn(max = MaxColumn)
                .fillMaxSize()
                .padding(top = 64.dp, bottom = 40.dp)
                .padding(horizontal = 22.dp),
        ) {
            itemsIndexed(section.blocks) { index, block ->
                Box(Modifier.fillMaxWidth().padding(top = gaps[index])) {
                    BlockText(
                        block = block,
                        colors = colors,
                        family = family,
                        bodySize = prefs.textSize,
                        onLayout = { layouts[index] = it },
                    )
                }
            }
            item(key = "end") {
                SectionEnd(
                    state = state,
                    colors = colors,
                    onNext = vm::nextSection,
                )
            }
        }
    }
}

/** Where the top of the page is: the first line that is at least half visible. */
private fun anchorAt(
    section: ReaderSection,
    firstIndex: Int,
    firstOffset: Float,
    layouts: Map<Int, TextLayoutResult>,
    gapPx: (Int) -> Float,
): ReadingAnchor {
    val blocks = section.blocks
    if (blocks.isEmpty()) return ReadingAnchor(section.index, 0, 0)
    if (firstIndex >= blocks.size) return ReadingAnchor(section.index, blocks.size - 1, 0)
    val layout = layouts[firstIndex] ?: return ReadingAnchor(section.index, firstIndex, 0)
    val y = firstOffset - gapPx(firstIndex)
    if (y <= 0f || layout.lineCount == 0) return ReadingAnchor(section.index, firstIndex, 0)
    var line = layout.getLineForVerticalPosition(y)
    val half = (layout.getLineBottom(line) - layout.getLineTop(line)) / 2f
    if (layout.getLineBottom(line) - y < half) line++
    if (line >= layout.lineCount) {
        return if (firstIndex + 1 < blocks.size) {
            ReadingAnchor(section.index, firstIndex + 1, 0)
        } else {
            ReadingAnchor(section.index, firstIndex, layout.getLineStart(layout.lineCount - 1))
        }
    }
    return ReadingAnchor(section.index, firstIndex, layout.getLineStart(line))
}

private fun topGap(block: ReaderBlock, textSize: Int): Dp {
    val unit = textSize * 0.9f
    val level = block.headingLevel
    return when {
        level != null -> (unit * if (level <= 2) 2.2f else 1.6f).dp
        block.kind == "hr" -> (unit * 1.2f).dp
        block.kind == "li" -> (unit * 0.35f).dp
        else -> (unit * 0.75f).dp
    }
}

@Composable
private fun BlockText(
    block: ReaderBlock,
    colors: ReaderColors,
    family: FontFamily,
    bodySize: Int,
    onLayout: (TextLayoutResult) -> Unit,
) {
    val level = block.headingLevel
    val body = TextStyle(
        color = colors.text,
        fontFamily = family,
        fontSize = bodySize.sp,
        lineHeight = (bodySize * 1.6f).sp,
    )
    when {
        level != null -> {
            val scale = when (level) {
                1 -> 1.55f
                2 -> 1.35f
                3 -> 1.2f
                else -> 1.08f
            }
            Text(
                block.text,
                modifier = Modifier.semantics { heading() },
                style = body.copy(
                    fontSize = (bodySize * scale).sp,
                    lineHeight = (bodySize * scale * 1.3f).sp,
                    fontWeight = FontWeight.SemiBold,
                ),
                onTextLayout = onLayout,
            )
        }
        block.kind == "hr" -> Box(
            Modifier
                .fillMaxWidth()
                .padding(vertical = 8.dp),
            contentAlignment = Alignment.Center,
        ) {
            Text("⁂", style = body.copy(color = colors.muted), onTextLayout = onLayout)
        }
        block.kind == "quote" -> Row {
            Box(
                Modifier
                    .width(3.dp)
                    .heightIn(min = 24.dp)
                    .background(colors.line),
            )
            Spacer(Modifier.width(14.dp))
            Text(block.text, style = body.copy(fontStyle = FontStyle.Italic), onTextLayout = onLayout)
        }
        block.kind == "li" -> Row {
            Text("•", style = body.copy(color = colors.muted))
            Spacer(Modifier.width(10.dp))
            Text(block.text, style = body, onTextLayout = onLayout)
        }
        block.kind == "pre" -> Text(
            block.text,
            style = body.copy(
                fontFamily = FontFamily.Monospace,
                fontSize = (bodySize * 0.82f).sp,
                lineHeight = (bodySize * 1.25f).sp,
            ),
            onTextLayout = onLayout,
        )
        else -> Text(block.text, style = body, onTextLayout = onLayout)
    }
}

@Composable
private fun SectionEnd(state: ReaderUiState, colors: ReaderColors, onNext: () -> Unit) {
    Column(
        Modifier
            .fillMaxWidth()
            .padding(top = 40.dp, bottom = 24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Box(Modifier.width(48.dp).height(1.dp).background(colors.line))
        Spacer(Modifier.height(20.dp))
        if (state.hasNext) {
            val next = state.sections.getOrNull(state.sectionIndex + 1)
            PrimaryButton(text = "Next section", onClick = onNext)
            next?.title?.takeIf { it.isNotBlank() }?.let {
                Spacer(Modifier.height(8.dp))
                Text(it, color = colors.muted, fontSize = 13.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
            }
        } else {
            Text("End of the book", color = colors.muted, fontSize = 14.sp)
        }
    }
}

@Composable
private fun ReaderTopBar(
    colors: ReaderColors,
    title: String,
    subtitle: String?,
    onBack: () -> Unit,
    actions: Boolean,
    onContents: () -> Unit,
    onText: () -> Unit,
) {
    Row(
        Modifier
            .fillMaxWidth()
            .background(colors.chrome)
            .heightIn(min = 56.dp)
            .padding(horizontal = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ChromeButton("‹", "Back", colors, onBack, fontSize = 30)
        Column(Modifier.weight(1f)) {
            Text(
                title,
                color = colors.text,
                fontSize = 16.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.semantics { heading() },
            )
            if (!subtitle.isNullOrBlank()) {
                Text(subtitle, color = colors.muted, fontSize = 12.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }
        if (actions) {
            ChromeButton("☰", "Contents", colors, onContents)
            ChromeButton("Aa", "Text settings", colors, onText, fontSize = 17)
        }
    }
}

@Composable
private fun ChromeButton(glyph: String, label: String, colors: ReaderColors, onClick: () -> Unit, fontSize: Int = 20) {
    Box(
        Modifier
            .size(48.dp)
            .clip(CircleShape)
            .clickable(onClickLabel = label, role = Role.Button, onClick = onClick)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        Text(glyph, color = colors.text, fontSize = fontSize.sp)
    }
}

@Composable
private fun ReaderBottomBar(state: ReaderUiState, colors: ReaderColors, onPrevious: () -> Unit, onNext: () -> Unit) {
    Column(
        Modifier
            .fillMaxWidth()
            .background(colors.chrome)
            .windowInsetsPadding(WindowInsets.navigationBars)
            .padding(horizontal = 8.dp, vertical = 6.dp),
    ) {
        ProgressTrack(state.percent / 100f, colors, Modifier.padding(horizontal = 12.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            ChromeTextButton("‹ Previous", "Previous section", colors, state.hasPrevious, onPrevious)
            Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
                Text("${percentText(state.percent)} by text", color = colors.text, fontSize = 13.sp)
                Text(
                    "Section ${state.sectionIndex + 1} of ${state.sections.size} · ${saveLabel(state.save)}",
                    color = colors.muted,
                    fontSize = 11.sp,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            ChromeTextButton("Next ›", "Next section", colors, state.hasNext, onNext)
        }
    }
}

@Composable
private fun ChromeTextButton(text: String, label: String, colors: ReaderColors, enabled: Boolean, onClick: () -> Unit) {
    Box(
        Modifier
            .heightIn(min = 48.dp)
            .clip(RoundedCornerShape(12.dp))
            .clickable(enabled = enabled, onClickLabel = label, role = Role.Button, onClick = onClick)
            .padding(horizontal = 12.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, color = if (enabled) colors.text else colors.muted.copy(alpha = 0.5f), fontSize = 14.sp)
    }
}

@Composable
private fun ProgressTrack(fraction: Float, colors: ReaderColors, modifier: Modifier = Modifier) {
    Box(
        modifier
            .fillMaxWidth()
            .padding(vertical = 6.dp)
            .height(3.dp)
            .clip(RoundedCornerShape(2.dp))
            .background(colors.line),
    ) {
        Box(
            Modifier
                .fillMaxWidth(fraction.coerceIn(0f, 1f))
                .height(3.dp)
                .background(colors.accent),
        )
    }
}

/** The quiet line shown while the controls are hidden. */
@Composable
private fun StatusLine(state: ReaderUiState, colors: ReaderColors, modifier: Modifier = Modifier) {
    val failed = state.save as? SaveStatus.Failed
    Row(
        modifier
            .fillMaxWidth()
            .windowInsetsPadding(WindowInsets.navigationBars)
            .padding(horizontal = 22.dp, vertical = 10.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(
            state.chapter.orEmpty(),
            color = colors.muted,
            fontSize = 11.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        Spacer(Modifier.width(12.dp))
        Text(
            if (failed != null) "Not saved" else percentText(state.percent),
            color = if (failed != null) Color(0xFFE17055) else colors.muted,
            fontSize = 11.sp,
        )
    }
}

private fun saveLabel(status: SaveStatus): String = when (status) {
    SaveStatus.Idle -> "position on SAIL"
    SaveStatus.Saved -> "saved to SAIL"
    is SaveStatus.Failed -> "not saved: ${status.message}"
}

@Composable
private fun ConflictCard(
    remotePercent: Float,
    colors: ReaderColors,
    onKeepHere: () -> Unit,
    onGoThere: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier
            .padding(ScreenGutter)
            .widthIn(max = 420.dp)
            .clip(RoundedCornerShape(20.dp))
            .background(colors.background)
            .border(1.dp, colors.line, RoundedCornerShape(20.dp))
            .padding(20.dp),
    ) {
        Text("Opened on another device", color = colors.text, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(6.dp))
        Text(
            "Another device saved this book at ${percentText(remotePercent)}. Nothing is saved from here until you choose.",
            color = colors.muted,
            fontSize = 14.sp,
            lineHeight = 20.sp,
        )
        Spacer(Modifier.height(16.dp))
        PrimaryButton(text = "Go to ${percentText(remotePercent)}", onClick = onGoThere, modifier = Modifier.fillMaxWidth())
        Spacer(Modifier.height(8.dp))
        SecondaryButton(text = "Keep reading here", onClick = onKeepHere, modifier = Modifier.fillMaxWidth())
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ContentsSheet(
    entries: List<TocEntry>,
    current: ReadingAnchor,
    colors: ReaderColors,
    onPick: (ReadingAnchor) -> Unit,
    onDismiss: () -> Unit,
) {
    val here = entries.indexOfLast {
        it.anchor.section < current.section ||
            (it.anchor.section == current.section && it.anchor.block <= current.block)
    }
    val list = rememberLazyListState(initialFirstVisibleItemIndex = (here - 2).coerceAtLeast(0))
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = colors.background,
        contentColor = colors.text,
    ) {
        Text(
            "Contents",
            color = colors.text,
            fontSize = 17.sp,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier.padding(horizontal = 20.dp).semantics { heading() },
        )
        Spacer(Modifier.height(8.dp))
        LazyColumn(state = list, contentPadding = PaddingValues(bottom = 24.dp)) {
            itemsIndexed(entries) { index, entry ->
                val selected = index == here
                Row(
                    Modifier
                        .fillMaxWidth()
                        .heightIn(min = 48.dp)
                        .clickable(onClickLabel = "Read ${entry.title}", role = Role.Button) { onPick(entry.anchor) }
                        .semantics { if (selected) stateDescription = "Reading here" }
                        .padding(start = (20 + entry.depth * 16).dp, end = 20.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        entry.title,
                        color = if (selected) colors.accent else colors.text,
                        fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
                        fontSize = 15.sp,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun TextSheet(
    prefs: ReaderPrefs,
    colors: ReaderColors,
    onChange: ((ReaderPrefs) -> ReaderPrefs) -> Unit,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        containerColor = colors.background,
        contentColor = colors.text,
    ) {
        Column(Modifier.padding(start = 20.dp, end = 20.dp, bottom = 28.dp)) {
            Text("Text", color = colors.text, fontSize = 17.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.semantics { heading() })
            Spacer(Modifier.height(14.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                ReaderTheme.entries.forEach { theme ->
                    val selected = theme == prefs.theme
                    Box(
                        Modifier
                            .weight(1f)
                            .heightIn(min = 56.dp)
                            .clip(RoundedCornerShape(14.dp))
                            .background(Color(theme.background))
                            .border(if (selected) 2.dp else 1.dp, if (selected) colors.accent else colors.line, RoundedCornerShape(14.dp))
                            .clickable(onClickLabel = "Use ${theme.label}", role = Role.RadioButton) { onChange { it.copy(theme = theme) } }
                            .semantics { stateDescription = if (selected) "Selected" else "Not selected" },
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(theme.label, color = Color(theme.text), fontSize = 14.sp)
                    }
                }
            }
            Spacer(Modifier.height(18.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                SizeButton("A−", "Smaller text", colors, prefs.textSize > ReaderPrefs.MIN_TEXT_SIZE) {
                    onChange { it.copy(textSize = it.textSize - 1) }
                }
                Text(
                    "${prefs.textSize} pt",
                    color = colors.text,
                    fontSize = 15.sp,
                    modifier = Modifier.weight(1f).semantics { contentDescription = "Text size ${prefs.textSize}" },
                    textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                )
                SizeButton("A+", "Larger text", colors, prefs.textSize < ReaderPrefs.MAX_TEXT_SIZE) {
                    onChange { it.copy(textSize = it.textSize + 1) }
                }
            }
            Spacer(Modifier.height(10.dp))
            ToggleRow("Serif typeface", prefs.serif, colors) { on -> onChange { it.copy(serif = on) } }
            ToggleRow("Keep the screen on", prefs.keepScreenOn, colors) { on -> onChange { it.copy(keepScreenOn = on) } }
        }
    }
}

@Composable
private fun SizeButton(text: String, label: String, colors: ReaderColors, enabled: Boolean, onClick: () -> Unit) {
    Box(
        Modifier
            .size(56.dp, 48.dp)
            .clip(RoundedCornerShape(14.dp))
            .border(1.dp, colors.line, RoundedCornerShape(14.dp))
            .clickable(enabled = enabled, onClickLabel = label, role = Role.Button, onClick = onClick)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        Text(text, color = if (enabled) colors.text else colors.muted.copy(alpha = 0.5f), fontSize = 16.sp)
    }
}

@Composable
private fun ToggleRow(label: String, checked: Boolean, colors: ReaderColors, onChange: (Boolean) -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 52.dp)
            .clickable(role = Role.Switch) { onChange(!checked) },
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, color = colors.text, fontSize = 15.sp, modifier = Modifier.weight(1f))
        Switch(
            checked = checked,
            onCheckedChange = null,
            colors = SwitchDefaults.colors(checkedTrackColor = colors.accent),
        )
    }
}

/** Left third, right third and middle taps; scroll drags pass through to the list. */
private fun Modifier.pointerInputTaps(onLeft: () -> Unit, onRight: () -> Unit, onMiddle: () -> Unit): Modifier =
    this.then(
        Modifier.pointerInput(Unit) {
            detectTapGestures { offset ->
                val third = size.width / 3f
                when {
                    offset.x < third -> onLeft()
                    offset.x > third * 2 -> onRight()
                    else -> onMiddle()
                }
            }
        },
    )
