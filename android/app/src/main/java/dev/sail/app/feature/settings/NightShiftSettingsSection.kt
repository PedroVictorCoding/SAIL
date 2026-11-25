package dev.sail.app.feature.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
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
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.sail.app.core.LocalAppContainer
import dev.sail.app.core.design.FlatCard
import dev.sail.app.core.design.PrimaryButton
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.design.SecondaryButton
import dev.sail.app.core.net.ApiResult
import dev.sail.app.feature.nightshift.NightShiftSettings
import kotlinx.coroutines.launch

@Composable
fun NightShiftSettingsSection() {
    val api = LocalAppContainer.current.api
    val scope = rememberCoroutineScope()

    var settings by remember { mutableStateOf(NightShiftSettings()) }
    var startText by remember { mutableStateOf("1") }
    var endText by remember { mutableStateOf("7") }
    var maxItemsText by remember { mutableStateOf("5") }
    var externalMaxText by remember { mutableStateOf("0") }
    var status by remember { mutableStateOf<String?>(null) }
    var saving by remember { mutableStateOf(false) }

    LaunchedEffect(Unit) {
        when (val res = api.nightShiftState()) {
            is ApiResult.Ok -> {
                val s = res.data.settings
                settings = s
                startText = s.windowStartHour.toString()
                endText = s.windowEndHour.toString()
                maxItemsText = s.maxItems.toString()
                externalMaxText = s.externalSolvesPerNight.toString()
            }
            is ApiResult.Err -> status = res.message
        }
    }

    FlatCard {
        Text("Night Shift", color = SailColors.Text, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(6.dp))

        SettingSwitchRow(
            label = "Enable Night Shift",
            checked = settings.enabled,
            onCheckedChange = { settings = settings.copy(enabled = it) },
        )
        SettingSwitchRow(
            label = "Use my PC when ready",
            checked = settings.useMesh,
            onCheckedChange = { settings = settings.copy(useMesh = it) },
        )
        SettingSwitchRow(
            label = "Morning greeting",
            checked = settings.morningGreeting,
            onCheckedChange = { settings = settings.copy(morningGreeting = it) },
        )

        Spacer(Modifier.height(6.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
            OutlinedTextField(
                value = startText,
                onValueChange = { startText = it.filter(Char::isDigit).take(2) },
                label = { Text("Start hour") },
                modifier = Modifier.weight(1f),
            )
            OutlinedTextField(
                value = endText,
                onValueChange = { endText = it.filter(Char::isDigit).take(2) },
                label = { Text("End hour") },
                modifier = Modifier.weight(1f),
            )
            OutlinedTextField(
                value = maxItemsText,
                onValueChange = { maxItemsText = it.filter(Char::isDigit).take(2) },
                label = { Text("Max jobs") },
                modifier = Modifier.weight(1f),
            )
        }

        Spacer(Modifier.height(6.dp))
        OutlinedTextField(
            value = externalMaxText,
            onValueChange = { externalMaxText = it.filter(Char::isDigit).take(1) },
            label = { Text("External help per night (0–3)") },
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(6.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            listOf(null to "Off", "claude_cli" to "Claude Code", "codex_cli" to "Codex").forEach { (id, label) ->
                val selected = settings.externalProvider == id
                if (selected) {
                    PrimaryButton(label, onClick = { settings = settings.copy(externalProvider = id) })
                } else {
                    SecondaryButton(label, onClick = { settings = settings.copy(externalProvider = id) })
                }
            }
        }

        status?.let {
            Spacer(Modifier.height(6.dp))
            Text(it, color = SailColors.TextMuted, fontSize = 12.sp)
        }

        Spacer(Modifier.height(8.dp))
        PrimaryButton(
            text = if (saving) "Saving…" else "Save Night Shift settings",
            enabled = !saving,
            onClick = {
                val payload = settings.copy(
                    includeForgeReadyIssues = false,
                    windowStartHour = (startText.toIntOrNull() ?: 1).coerceIn(0, 23),
                    windowEndHour = (endText.toIntOrNull() ?: 7).coerceIn(0, 23),
                    maxItems = (maxItemsText.toIntOrNull() ?: 5).coerceIn(1, 10),
                    externalSolvesPerNight = (externalMaxText.toIntOrNull() ?: 0).coerceIn(0, 3),
                )
                scope.launch {
                    saving = true
                    status = null
                    when (val res = api.saveNightShiftSettings(payload)) {
                        is ApiResult.Ok -> {
                            settings = res.data
                            status = "Saved."
                        }
                        is ApiResult.Err -> status = res.message
                    }
                    saving = false
                }
            },
        )
    }
}

@Composable
private fun SettingSwitchRow(
    label: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, color = SailColors.Text, fontSize = 14.sp, modifier = Modifier.weight(1f))
        Switch(checked = checked, onCheckedChange = onCheckedChange)
    }
}
