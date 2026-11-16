package dev.sail.app.feature.dock

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.content.pm.PackageManager
import android.view.WindowManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.sail.app.core.LocalAppContainer
import dev.sail.app.core.design.FlatCard
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.sailViewModel
import kotlinx.coroutines.delay
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

@Composable
fun DockScreen(onExit: () -> Unit) {
    val container = LocalAppContainer.current
    val vm = sailViewModel { DockViewModel(it.api, it.heartbeats) }
    val state by vm.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current

    var hasMic by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
                PackageManager.PERMISSION_GRANTED,
        )
    }
    val micPermLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        hasMic = granted
        if (granted) vm.startListening(true)
    }

    LaunchedEffect(Unit) {
        if (!hasMic) {
            micPermLauncher.launch(Manifest.permission.RECORD_AUDIO)
        }
    }

    // Keep screen on while charging.
    DisposableEffect(state.charging, context) {
        val window = context.findActivity()?.window
        if (state.charging) {
            window?.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        } else {
            window?.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
        onDispose {
            window?.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
    }

    // Open VoiceSession + mic in foreground; close in onStop.
    DisposableEffect(lifecycleOwner, hasMic) {
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_START -> vm.onForegroundStart(hasMic)
                Lifecycle.Event.ON_STOP -> vm.onForegroundStop()
                else -> Unit
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose {
            lifecycleOwner.lifecycle.removeObserver(observer)
            vm.onForegroundStop()
        }
    }

    var clockText by remember { mutableStateOf(formatClock()) }
    LaunchedEffect(Unit) {
        while (true) {
            clockText = formatClock()
            delay(15_000L)
        }
    }

    val isAsleep = state.place == Place.ASLEEP

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(SailColors.Base)
            .clickable(enabled = isAsleep) { container.heartbeats.noteTouch() }
            .padding(20.dp),
    ) {
        if (isAsleep) {
            AsleepClockView(
                clockText = clockText,
                listening = state.listening,
                onExit = onExit,
            )
        } else {
            BoxWithConstraints(Modifier.fillMaxSize()) {
                val landscape = maxWidth > maxHeight
                if (landscape) {
                    Row(
                        modifier = Modifier.fillMaxSize(),
                        horizontalArrangement = Arrangement.spacedBy(24.dp),
                    ) {
                        Column(
                            modifier = Modifier
                                .weight(0.44f)
                                .fillMaxHeight(),
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.Center,
                        ) {
                            DockOrbCluster(
                                clockText = clockText,
                                state = state,
                                onTapOrb = { vm.togglePushToTalk(hasMic) },
                                onStopListening = vm::stopListening,
                                onStartListening = { vm.startListening(hasMic) },
                                onExit = onExit,
                            )
                        }
                        DockContextColumn(
                            state = state,
                            onDismissGreeting = vm::dismissGreeting,
                            onFeedback = vm::sendFeedback,
                            modifier = Modifier
                                .weight(0.56f)
                                .fillMaxHeight(),
                        )
                    }
                } else {
                    Column(
                        modifier = Modifier.fillMaxSize(),
                        verticalArrangement = Arrangement.spacedBy(16.dp),
                    ) {
                        DockOrbCluster(
                            clockText = clockText,
                            state = state,
                            onTapOrb = { vm.togglePushToTalk(hasMic) },
                            onStopListening = vm::stopListening,
                            onStartListening = { vm.startListening(hasMic) },
                            onExit = onExit,
                        )
                        DockContextColumn(
                            state = state,
                            onDismissGreeting = vm::dismissGreeting,
                            onFeedback = vm::sendFeedback,
                            modifier = Modifier.weight(1f),
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun AsleepClockView(
    clockText: String,
    listening: Boolean,
    onExit: () -> Unit,
) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .alpha(0.45f),
        contentAlignment = Alignment.Center,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Text(
                text = clockText,
                fontSize = 64.sp,
                fontWeight = FontWeight.Light,
                color = SailColors.Text,
            )
            Spacer(Modifier.height(8.dp))
            Text(
                text = if (listening) "Quiet hours · Mic listening" else "Quiet hours",
                color = SailColors.TextMuted,
                fontSize = 14.sp,
            )
            Spacer(Modifier.height(16.dp))
            TextButton(onClick = onExit) {
                Text("Exit dock", color = SailColors.TextMuted)
            }
        }
    }
}

@Composable
private fun DockOrbCluster(
    clockText: String,
    state: DockUiState,
    onTapOrb: () -> Unit,
    onStopListening: () -> Unit,
    onStartListening: () -> Unit,
    onExit: () -> Unit,
) {
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            if (state.listening) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier
                        .clip(RoundedCornerShape(999.dp))
                        .background(Color(0x2819B5BC))
                        .padding(horizontal = 12.dp, vertical = 5.dp),
                ) {
                    Box(
                        Modifier
                            .size(8.dp)
                            .clip(CircleShape)
                            .background(Color(0xFF19B5BC)),
                    )
                    Spacer(Modifier.width(6.dp))
                    Text(
                        text = when (state.handsFreeState) {
                            "follow_up" -> "Go ahead"
                            else -> "Listening for \"SAIL\""
                        },
                        color = SailColors.Text,
                        fontSize = 12.sp,
                    )
                }
            }
            if (state.charging) {
                Text(
                    text = "Charging",
                    color = SailColors.TextMuted,
                    fontSize = 12.sp,
                )
            }
        }

        DockOrb(
            orbState = state.orbState,
            onClick = onTapOrb,
        )

        Text(
            text = clockText,
            fontSize = 44.sp,
            fontWeight = FontWeight.Light,
            color = SailColors.Text,
        )

        Row(
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (state.listening) {
                Button(
                    onClick = onStopListening,
                    colors = ButtonDefaults.buttonColors(
                        containerColor = SailColors.SurfaceRaised,
                        contentColor = SailColors.Text,
                    ),
                ) {
                    Text("Stop listening")
                }
            } else {
                Button(onClick = onStartListening) {
                    Text("Start listening")
                }
            }
            OutlinedButton(onClick = onExit) {
                Text("Exit dock")
            }
        }

        if (!state.error.isNullOrBlank()) {
            Text(
                text = state.error,
                color = Color(0xFFED7275),
                fontSize = 13.sp,
            )
        }
    }
}

@Composable
private fun DockOrb(
    orbState: String,
    onClick: () -> Unit,
) {
    val transition = rememberInfiniteTransition(label = "dockOrb")
    val pulse by transition.animateFloat(
        initialValue = 0.88f,
        targetValue = 1.0f,
        animationSpec = infiniteRepeatable(tween(2400), RepeatMode.Reverse),
        label = "dockOrbPulse",
    )
    val orbColor = when (orbState) {
        "speaking" -> Color(0xFFFF7A45)
        "thinking" -> Color(0xFF8567FF)
        "listening" -> Color(0xFF19B5BC)
        else -> Color(0xFF496F9D)
    }

    Box(
        modifier = Modifier
            .size(132.dp)
            .clip(CircleShape)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics { contentDescription = "SAIL orb, tap to talk" },
        contentAlignment = Alignment.Center,
    ) {
        Canvas(Modifier.fillMaxSize()) {
            drawCircle(
                brush = Brush.radialGradient(
                    colors = listOf(
                        orbColor.copy(alpha = 0.55f * pulse),
                        orbColor.copy(alpha = 0.16f),
                        Color.Transparent,
                    ),
                ),
            )
            drawCircle(
                color = orbColor.copy(alpha = 0.85f),
                radius = size.minDimension * 0.24f * pulse,
            )
        }
    }
}

@Composable
private fun DockContextColumn(
    state: DockUiState,
    onDismissGreeting: () -> Unit,
    onFeedback: (String, String) -> Unit,
    modifier: Modifier = Modifier,
) {
    LazyColumn(
        modifier = modifier,
        contentPadding = PaddingValues(vertical = 4.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        state.greeting?.let { greeting ->
            item(key = "greeting") {
                FlatCard {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.Top,
                    ) {
                        Text(
                            text = greeting.spoken,
                            style = MaterialTheme.typography.bodyLarge,
                            color = SailColors.Text,
                            modifier = Modifier.weight(1f),
                        )
                        TextButton(onClick = onDismissGreeting) {
                            Text("Dismiss")
                        }
                    }
                }
            }
        }

        state.nextReminder?.let { reminder ->
            item(key = "next_reminder") {
                FlatCard {
                    Text("Next reminder", color = SailColors.TextMuted, fontSize = 12.sp)
                    Spacer(Modifier.height(4.dp))
                    Text(reminder.text, color = SailColors.Text, fontSize = 15.sp)
                }
            }
        }

        state.runningTaskGoal?.let { goal ->
            item(key = "running_task") {
                FlatCard {
                    Text("Running task", color = SailColors.TextMuted, fontSize = 12.sp)
                    Spacer(Modifier.height(4.dp))
                    Text(goal, color = SailColors.Text, fontSize = 15.sp)
                }
            }
        }

        items(state.announcements, key = { it.itemId }) { item ->
            DockAnnouncementCard(item = item, onFeedback = onFeedback)
        }
    }
}

@Composable
private fun DockAnnouncementCard(
    item: DockAnnouncement,
    onFeedback: (String, String) -> Unit,
) {
    var showReasons by remember(item.itemId) { mutableStateOf(false) }
    FlatCard {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = listOf(item.source, item.kind).filter { it.isNotBlank() }.joinToString(" · "),
                color = SailColors.TextMuted,
                fontSize = 12.sp,
            )
            TextButton(onClick = { showReasons = !showReasons }) {
                Text(if (showReasons) "Hide why" else "Why?")
            }
        }
        Text(
            text = item.title,
            color = SailColors.Text,
            fontWeight = FontWeight.SemiBold,
            fontSize = 16.sp,
        )
        if (item.body.isNotBlank()) {
            Spacer(Modifier.height(4.dp))
            Text(text = item.body, color = SailColors.TextMuted, fontSize = 14.sp)
        }
        if (showReasons && item.reasons.isNotEmpty()) {
            Spacer(Modifier.height(6.dp))
            item.reasons.forEach { reason ->
                Text("• $reason", color = SailColors.TextMuted, fontSize = 12.sp)
            }
        }
        Spacer(Modifier.height(8.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(onClick = { onFeedback(item.itemId, "useful") }) {
                Text("Useful")
            }
            OutlinedButton(onClick = { onFeedback(item.itemId, "not_now") }) {
                Text("Not now")
            }
            TextButton(onClick = { onFeedback(item.itemId, "mute") }) {
                Text("Mute")
            }
        }
    }
}

private fun formatClock(): String =
    SimpleDateFormat("HH:mm", Locale.getDefault()).format(Date())

private tailrec fun Context.findActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.findActivity()
    else -> null
}
