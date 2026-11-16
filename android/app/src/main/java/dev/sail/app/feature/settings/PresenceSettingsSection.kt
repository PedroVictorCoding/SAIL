package dev.sail.app.feature.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Checkbox
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.sail.app.core.LocalAppContainer
import dev.sail.app.core.design.FlatCard
import dev.sail.app.core.design.PrimaryButton
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.getOrNull
import dev.sail.app.feature.dock.Place
import dev.sail.app.feature.dock.PresenceSettings
import dev.sail.app.feature.dock.SurfaceKind
import kotlinx.coroutines.launch
import kotlin.math.roundToLong

@Composable
fun PresenceSettingsSection() {
    val container = LocalAppContainer.current
    val scope = rememberCoroutineScope()
    val openDockOnCharge by container.heartbeats.openDockWhenCharging.collectAsStateWithLifecycle()

    var place by remember { mutableStateOf(Place.UNKNOWN) }
    var speakAnnouncements by remember { mutableStateOf(true) }
    var speakWeb by remember { mutableStateOf(true) }
    var speakDock by remember { mutableStateOf(true) }
    var speakPhone by remember { mutableStateOf(false) }
    var greetOnArrival by remember { mutableStateOf(true) }
    var telegramWhenAway by remember { mutableStateOf(false) }
    var awayMinsText by remember { mutableStateOf("10") }
    var greetMinsText by remember { mutableStateOf("45") }
    var maxSpokenText by remember { mutableStateOf("3") }
    var statusText by remember { mutableStateOf<String?>(null) }
    var statusError by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }

    fun applySettings(s: PresenceSettings) {
        speakAnnouncements = s.speakAnnouncements
        speakWeb = SurfaceKind.WEB in s.speakOn
        speakDock = SurfaceKind.DOCK in s.speakOn
        speakPhone = SurfaceKind.PHONE in s.speakOn
        greetOnArrival = s.greetOnArrival
        telegramWhenAway = s.telegramWhenAway
        awayMinsText = (s.awayAfterSecs / 60L).coerceAtLeast(1L).toString()
        greetMinsText = (s.greetAfterAwaySecs / 60L).coerceAtLeast(1L).toString()
        maxSpokenText = s.maxSpokenPerHour.toString()
    }

    LaunchedEffect(Unit) {
        container.api.presenceState().getOrNull()?.let { place = it.place }
        container.api.presenceSettings().getOrNull()?.let { applySettings(it) }
    }

    HorizontalDivider(color = SailColors.Line)
    Text("Presence", fontWeight = FontWeight.SemiBold, fontSize = 18.sp, color = SailColors.Text)

    FlatCard {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("Current place", color = SailColors.TextMuted, fontSize = 14.sp)
            Text(placeLabel(place), color = SailColors.Text, fontWeight = FontWeight.Medium)
        }
    }

    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text("Open dock mode when charging", color = SailColors.Text, fontSize = 15.sp)
            Text(
                "Applies while the app is in the foreground.",
                color = SailColors.TextMuted,
                fontSize = 12.sp,
            )
        }
        Switch(
            checked = openDockOnCharge,
            onCheckedChange = { container.heartbeats.setOpenDockWhenCharging(it) },
        )
    }

    PresenceSwitchRow(
        title = "Speak announcements",
        checked = speakAnnouncements,
        onCheckedChange = { speakAnnouncements = it },
    )
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        SurfaceCheck("Desk", speakWeb) { speakWeb = it }
        SurfaceCheck("Dock", speakDock) { speakDock = it }
        SurfaceCheck("Phone", speakPhone) { speakPhone = it }
    }
    PresenceSwitchRow(
        title = "Greet me when I come back",
        checked = greetOnArrival,
        onCheckedChange = { greetOnArrival = it },
    )
    PresenceSwitchRow(
        title = "Send urgent alerts to Telegram when away",
        checked = telegramWhenAway,
        onCheckedChange = { telegramWhenAway = it },
    )

    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        OutlinedTextField(
            value = awayMinsText,
            onValueChange = { awayMinsText = it },
            label = { Text("Away (min)") },
            singleLine = true,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
            modifier = Modifier.weight(1f),
        )
        OutlinedTextField(
            value = greetMinsText,
            onValueChange = { greetMinsText = it },
            label = { Text("Greet after (min)") },
            singleLine = true,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
            modifier = Modifier.weight(1f),
        )
        OutlinedTextField(
            value = maxSpokenText,
            onValueChange = { maxSpokenText = it },
            label = { Text("Spoken / hr") },
            singleLine = true,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
            modifier = Modifier.weight(1f),
        )
    }

    PrimaryButton(
        text = if (busy) "Saving…" else "Save presence settings",
        enabled = !busy,
        onClick = {
            busy = true
            statusText = null
            val speakOn = buildList {
                if (speakWeb) add(SurfaceKind.WEB)
                if (speakDock) add(SurfaceKind.DOCK)
                if (speakPhone) add(SurfaceKind.PHONE)
            }
            val payload = PresenceSettings(
                awayAfterSecs = ((awayMinsText.toDoubleOrNull() ?: 10.0) * 60.0).roundToLong().coerceAtLeast(60L),
                greetOnArrival = greetOnArrival,
                greetAfterAwaySecs = ((greetMinsText.toDoubleOrNull() ?: 45.0) * 60.0).roundToLong().coerceAtLeast(60L),
                speakAnnouncements = speakAnnouncements,
                speakOn = speakOn,
                maxSpokenPerHour = maxSpokenText.toIntOrNull() ?: 3,
                telegramWhenAway = telegramWhenAway,
            )
            scope.launch {
                val res = container.api.savePresenceSettings(payload)
                busy = false
                when (res) {
                    is ApiResult.Ok -> {
                        applySettings(res.data)
                        statusText = "Presence preferences saved."
                        statusError = false
                    }
                    is ApiResult.Err -> {
                        statusText = res.message
                        statusError = true
                    }
                }
            }
        },
    )

    statusText?.let { msg ->
        Text(
            text = msg,
            color = if (statusError) SailColors.Error else SailColors.Good,
            fontSize = 13.sp,
        )
    }
}

@Composable
private fun PresenceSwitchRow(
    title: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(title, color = SailColors.Text, fontSize = 15.sp, modifier = Modifier.weight(1f))
        Switch(checked = checked, onCheckedChange = onCheckedChange)
    }
}

@Composable
private fun SurfaceCheck(
    label: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Checkbox(checked = checked, onCheckedChange = onCheckedChange)
        Text(label, color = SailColors.Text, fontSize = 14.sp)
    }
}

private fun placeLabel(place: Place): String = when (place) {
    Place.DESK -> "You're at the desk"
    Place.DOCK -> "You're at the dock"
    Place.PHONE -> "You're on your phone"
    Place.AWAY -> "Away"
    Place.ASLEEP -> "Quiet hours (asleep)"
    Place.UNKNOWN -> "Unknown"
}
