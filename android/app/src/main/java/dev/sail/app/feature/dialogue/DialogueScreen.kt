package dev.sail.app.feature.dialogue

import android.Manifest
import android.content.pm.PackageManager
import android.util.Base64
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.sail.app.core.design.Eyebrow
import dev.sail.app.core.design.FlatCard
import dev.sail.app.core.design.GlowCard
import dev.sail.app.core.design.Hue
import dev.sail.app.core.design.MarkdownText
import dev.sail.app.core.design.PrimaryButton
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.design.SailIcon
import dev.sail.app.core.design.SailIconImage
import dev.sail.app.core.design.SailPill
import dev.sail.app.core.design.SailTopBar
import dev.sail.app.core.design.ScreenGutter
import dev.sail.app.core.design.SecondaryButton
import dev.sail.app.core.sailViewModel

@Composable
fun DialogueScreen() {
    val vm = sailViewModel { DialogueViewModel(it.api) }
    val state by vm.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    var draft by rememberSaveable { mutableStateOf("") }
    var attachment by remember { mutableStateOf<ByteArray?>(null) }
    var snapshotFor by remember { mutableStateOf<String?>(null) }

    val micPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) vm.startMic()
    }
    val pickPhoto = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri != null) attachment = Jpeg.fromUri(context, uri)
    }
    val takePicture = rememberLauncherForActivityResult(ActivityResultContracts.TakePicturePreview()) { bitmap ->
        val request = snapshotFor ?: return@rememberLauncherForActivityResult
        snapshotFor = null
        // Cancelled camera: leave the request open so the owner can retry or decline.
        if (bitmap != null) vm.answerSnapshot(request, Jpeg.fromBitmap(bitmap))
    }
    val cameraPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) takePicture.launch(null) else snapshotFor = null
    }

    fun toggleMic() {
        if (state.micOn) {
            vm.stopMic()
        } else if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
            vm.startMic()
        } else {
            micPermission.launch(Manifest.permission.RECORD_AUDIO)
        }
    }

    fun takeSnapshot(requestId: String) {
        snapshotFor = requestId
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
            takePicture.launch(null)
        } else {
            cameraPermission.launch(Manifest.permission.CAMERA)
        }
    }

    Column(Modifier.fillMaxSize()) {
        SailTopBar(
            title = "Dialogue",
            subtitle = "SAIL",
            actions = { SailPill(state.phase.label, dot = phaseColor(state.phase)) },
        )
        val list = rememberLazyListState()
        LaunchedEffect(state.items.size, (state.items.lastOrNull() as? ChatItem.Sail)?.text?.length) {
            if (state.items.isNotEmpty()) list.animateScrollToItem(state.items.size - 1)
        }
        LazyColumn(
            state = list,
            modifier = Modifier.weight(1f).fillMaxWidth(),
            contentPadding = PaddingValues(horizontal = ScreenGutter, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            if (state.items.isEmpty()) {
                item {
                    Text(
                        "Ask SAIL anything, by voice or text. Replies, tool results and approvals show up here.",
                        color = SailColors.TextMuted,
                        fontSize = 14.sp,
                        modifier = Modifier.padding(top = 24.dp),
                    )
                }
            }
            items(state.items, key = { it.key }) { item ->
                when (item) {
                    is ChatItem.User -> UserBubble(item)
                    is ChatItem.Sail -> SailReply(item)
                    is ChatItem.Visual -> VisualCard(item)
                    is ChatItem.ToolOutcome -> ToolFailure(item)
                    is ChatItem.Approval -> ApprovalCard(item, onResolve = vm::resolveApproval)
                    is ChatItem.Snapshot -> SnapshotCard(
                        item,
                        onTake = { takeSnapshot(item.requestId) },
                        onDecline = { vm.answerSnapshot(item.requestId, null) },
                    )
                }
            }
        }
        StatusStrip(state, onDismissError = vm::dismissError)
        Composer(
            draft = draft,
            onDraft = { draft = it },
            attachment = attachment,
            onClearAttachment = { attachment = null },
            micOn = state.micOn,
            busy = state.turnInProgress || state.phase == Phase.Speaking,
            connected = state.phase != Phase.Disconnected,
            onAttach = { pickPhoto.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly)) },
            onSend = {
                vm.sendText(draft, attachment)
                draft = ""
                attachment = null
            },
            onMic = ::toggleMic,
            onEndTurn = vm::endTurn,
            onStop = vm::interrupt,
        )
    }
}

private fun phaseColor(phase: Phase): Color = when (phase) {
    Phase.Disconnected -> SailColors.Warning
    Phase.Idle -> SailColors.Good
    Phase.Listening -> SailColors.Listening
    Phase.Transcribing, Phase.Thinking -> SailColors.Thinking
    Phase.Working -> SailColors.Executing
    Phase.Speaking -> SailColors.Speaking
    Phase.Approval, Phase.Holding -> SailColors.Warning
}

@Composable
private fun UserBubble(item: ChatItem.User) {
    Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.CenterEnd) {
        Column(
            modifier = Modifier
                .widthIn(max = 300.dp)
                .clip(RoundedCornerShape(18.dp, 18.dp, 4.dp, 18.dp))
                .background(Color(0xFF232C4A))
                .border(1.dp, SailColors.LineStrong, RoundedCornerShape(18.dp, 18.dp, 4.dp, 18.dp))
                .padding(horizontal = 14.dp, vertical = 10.dp),
        ) {
            item.imageJpeg?.let { bytes -> JpegImage(bytes, "Picture you sent") }
            if (item.text.isNotBlank()) Text(item.text, color = SailColors.Text, fontSize = 15.sp)
        }
    }
}

@Composable
private fun SailReply(item: ChatItem.Sail) {
    Column(Modifier.fillMaxWidth().semantics { if (item.done) liveRegion = LiveRegionMode.Polite }) {
        if (item.text.isBlank() && !item.done) {
            Text("…", color = SailColors.TextMuted, fontSize = 18.sp)
        } else {
            MarkdownText(item.text)
        }
    }
}

@Composable
private fun VisualCard(item: ChatItem.Visual) {
    GlowCard(hue = Hue.Dialogue, modifier = Modifier.fillMaxWidth()) {
        Eyebrow(
            when (item.type) {
                "image" -> "Canvas · Image"
                "json" -> "Canvas · Data"
                "mermaid" -> "Canvas · Diagram"
                else -> "Canvas · Visual"
            },
        )
        Spacer(Modifier.size(8.dp))
        when (item.type) {
            "image" -> {
                val bytes = remember(item.content) { dataUrlBytes(item.content) }
                if (bytes != null) JpegImage(bytes, "Image from SAIL") else Unsupported()
            }
            "json" -> Text(
                item.content,
                modifier = Modifier.horizontalScroll(rememberScrollState()),
                fontFamily = FontFamily.Monospace,
                fontSize = 12.sp,
                color = SailColors.Text,
                softWrap = false,
            )
            else -> Unsupported()
        }
    }
}

@Composable
private fun Unsupported() {
    Text(
        "This visual can't be drawn in the app yet. Open SAIL on a computer to see it.",
        color = SailColors.TextMuted,
        fontSize = 13.sp,
    )
}

@Composable
private fun JpegImage(bytes: ByteArray, description: String) {
    val bitmap = remember(bytes) { Jpeg.decode(bytes)?.asImageBitmap() }
    if (bitmap != null) {
        Image(
            bitmap = bitmap,
            contentDescription = description,
            contentScale = ContentScale.FillWidth,
            modifier = Modifier
                .fillMaxWidth()
                .padding(bottom = 6.dp)
                .clip(RoundedCornerShape(12.dp)),
        )
    }
}

private fun dataUrlBytes(content: String): ByteArray? {
    val base64 = content.substringAfter("base64,", missingDelimiterValue = "")
    if (base64.isEmpty()) return null
    return try {
        Base64.decode(base64, Base64.DEFAULT)
    } catch (error: IllegalArgumentException) {
        null
    }
}

@Composable
private fun ToolFailure(item: ChatItem.ToolOutcome) {
    FlatCard(Modifier.fillMaxWidth()) {
        Eyebrow("${VoiceProtocol.toolLabel(item.tool)} · failed", dot = SailColors.Error)
        if (item.error != null) {
            Spacer(Modifier.size(6.dp))
            Text(item.error, color = SailColors.TextMuted, fontSize = 13.sp)
        }
    }
}

@Composable
private fun ApprovalCard(item: ChatItem.Approval, onResolve: (String, Boolean) -> Unit) {
    GlowCard(hue = Hue.Warning, modifier = Modifier.fillMaxWidth()) {
        Eyebrow("Approval needed", dot = SailColors.Warning)
        Spacer(Modifier.size(6.dp))
        Text(
            listOfNotNull(item.tool, item.risk?.let { "$it risk" }).joinToString(" · ").ifEmpty { "A guarded action" },
            fontWeight = FontWeight.SemiBold,
        )
        item.rationale?.let { Text(it, color = SailColors.TextMuted, fontSize = 13.sp) }
        Spacer(Modifier.size(10.dp))
        when (item.status) {
            ApprovalStatus.Pending, ApprovalStatus.Sending -> Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                val enabled = item.status == ApprovalStatus.Pending
                PrimaryButton("Approve once", onClick = { onResolve(item.sessionId, true) }, enabled = enabled)
                SecondaryButton("Deny", onClick = { onResolve(item.sessionId, false) }, enabled = enabled)
            }
            ApprovalStatus.Approved -> Text("Approved once", color = SailColors.Good)
            ApprovalStatus.Denied -> Text("Denied", color = SailColors.TextMuted)
            ApprovalStatus.Resolved -> Text("Resolved", color = SailColors.TextMuted)
        }
    }
}

@Composable
private fun SnapshotCard(item: ChatItem.Snapshot, onTake: () -> Unit, onDecline: () -> Unit) {
    GlowCard(hue = Hue.Dialogue, modifier = Modifier.fillMaxWidth()) {
        Eyebrow(if (item.source == "screen") "SAIL wants to see your screen" else "SAIL wants to see your camera")
        Spacer(Modifier.size(6.dp))
        Text(
            if (item.source == "screen") {
                "Screen capture isn't available in the app. You can send a camera picture instead."
            } else {
                "Take one picture. It's sent to your SAIL server and analysed there."
            },
            color = SailColors.TextMuted,
            fontSize = 13.sp,
        )
        item.question?.let { Text("SAIL wants to know: “$it”", fontSize = 13.sp, modifier = Modifier.padding(top = 4.dp)) }
        Spacer(Modifier.size(10.dp))
        if (item.open) {
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                PrimaryButton("Take picture", onClick = onTake)
                SecondaryButton("No thanks", onClick = onDecline)
            }
        } else {
            Text("Answered", color = SailColors.TextMuted, fontSize = 13.sp)
        }
    }
}

@Composable
private fun StatusStrip(state: DialogueUiState, onDismissError: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(horizontal = ScreenGutter)) {
        state.error?.let { error ->
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(bottom = 6.dp)
                    .clip(RoundedCornerShape(12.dp))
                    .background(Color(0xCC2A1512))
                    .clickable(onClickLabel = "Dismiss", role = Role.Button, onClick = onDismissError)
                    .padding(horizontal = 12.dp, vertical = 8.dp)
                    .semantics { liveRegion = LiveRegionMode.Assertive },
            ) {
                Text(error, color = SailColors.Error, fontSize = 13.sp, modifier = Modifier.weight(1f))
                Text("✕", color = SailColors.TextMuted, modifier = Modifier.padding(start = 8.dp))
            }
        }
        val activity = state.activeTools.lastOrNull()?.let(VoiceProtocol::toolLabel)
            ?: state.liveTranscript?.let { "${if (state.micOn) "Listening" else "Heard"} · $it" }
        if (activity != null) {
            Text(
                activity,
                color = SailColors.TextMuted,
                fontSize = 13.sp,
                maxLines = 2,
                modifier = Modifier.padding(bottom = 6.dp).semantics { liveRegion = LiveRegionMode.Polite },
            )
        }
    }
}

@Composable
private fun Composer(
    draft: String,
    onDraft: (String) -> Unit,
    attachment: ByteArray?,
    onClearAttachment: () -> Unit,
    micOn: Boolean,
    busy: Boolean,
    connected: Boolean,
    onAttach: () -> Unit,
    onSend: () -> Unit,
    onMic: () -> Unit,
    onEndTurn: () -> Unit,
    onStop: () -> Unit,
) {
    Column(Modifier.fillMaxWidth().padding(horizontal = ScreenGutter, vertical = 8.dp)) {
        if (attachment != null) {
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(bottom = 6.dp)) {
                SailPill("Picture attached")
                Text(
                    "Remove",
                    color = SailColors.TextMuted,
                    fontSize = 13.sp,
                    modifier = Modifier
                        .clip(RoundedCornerShape(8.dp))
                        .clickable(role = Role.Button, onClick = onClearAttachment)
                        .padding(12.dp),
                )
            }
        }
        if (micOn) {
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp), modifier = Modifier.padding(bottom = 6.dp)) {
                SecondaryButton("Send turn", onClick = onEndTurn)
            }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            RoundButton(label = "Attach a picture", onClick = onAttach, enabled = connected) {
                Text("+", fontSize = 22.sp, color = SailColors.Text)
            }
            Spacer(Modifier.size(8.dp))
            OutlinedTextField(
                value = draft,
                onValueChange = onDraft,
                placeholder = { Text("Ask SAIL anything…") },
                modifier = Modifier.weight(1f).heightIn(min = 48.dp),
                shape = RoundedCornerShape(24.dp),
                maxLines = 5,
                enabled = connected,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                keyboardActions = KeyboardActions(onSend = { onSend() }),
                colors = OutlinedTextFieldDefaults.colors(
                    unfocusedBorderColor = SailColors.LineStrong,
                    focusedBorderColor = Color(0xFF8A7DFF),
                ),
            )
            Spacer(Modifier.size(8.dp))
            when {
                busy -> RoundButton(label = "Stop SAIL", onClick = onStop, tint = SailColors.Speaking) {
                    Box(Modifier.size(14.dp).clip(RoundedCornerShape(3.dp)).background(Color.White))
                }
                draft.isNotBlank() || attachment != null -> RoundButton(label = "Send", onClick = onSend, enabled = connected, tint = Color(0xFF8A7DFF)) {
                    Text("↑", fontSize = 20.sp, color = Color.White, fontWeight = FontWeight.Bold)
                }
                else -> RoundButton(
                    label = if (micOn) "Stop microphone" else "Start microphone",
                    onClick = onMic,
                    enabled = connected,
                    tint = if (micOn) SailColors.Listening else Color(0xFF19B5BC).copy(alpha = 0.55f),
                ) {
                    SailIconImage(SailIcon.Mic, contentDescription = null, size = 22.dp)
                }
            }
        }
    }
}

@Composable
private fun RoundButton(
    label: String,
    onClick: () -> Unit,
    enabled: Boolean = true,
    tint: Color = SailColors.SurfaceRaised,
    content: @Composable () -> Unit,
) {
    Box(
        modifier = Modifier
            .size(48.dp)
            .clip(CircleShape)
            .background(if (enabled) tint else tint.copy(alpha = 0.35f))
            .clickable(enabled = enabled, onClickLabel = label, role = Role.Button, onClick = onClick)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        content()
    }
}
