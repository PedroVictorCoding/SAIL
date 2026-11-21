package dev.sail.app.feature.calendar

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.sail.app.core.design.FlatCard
import dev.sail.app.core.design.PrimaryButton
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.design.SecondaryButton
import java.time.LocalDate

@Composable
fun CreateEventSheet(
    defaultDate: LocalDate,
    busy: Boolean,
    onSave: (EventDraft) -> Unit,
    onDismiss: () -> Unit,
) {
    var title by remember { mutableStateOf("") }
    var dateText by remember { mutableStateOf(defaultDate.toString()) }
    var allDay by remember { mutableStateOf(false) }
    var startTime by remember { mutableStateOf("09:00") }
    var endTime by remember { mutableStateOf("10:00") }
    var location by remember { mutableStateOf("") }
    var notes by remember { mutableStateOf("") }
    var repeat by remember { mutableStateOf(RepeatPreset.NONE) }

    FlatCard {
        Text("Add event", color = SailColors.Text, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(8.dp))
        OutlinedTextField(
            value = title,
            onValueChange = { if (it.length <= 200) title = it },
            label = { Text("Title") },
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(6.dp))
        OutlinedTextField(
            value = dateText,
            onValueChange = { dateText = it },
            label = { Text("Date (YYYY-MM-DD)") },
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(6.dp))
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("All-day", color = SailColors.Text, fontSize = 14.sp)
            Switch(checked = allDay, onCheckedChange = { allDay = it })
        }
        if (!allDay) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
                OutlinedTextField(
                    value = startTime,
                    onValueChange = { startTime = it },
                    label = { Text("Start") },
                    modifier = Modifier.weight(1f),
                )
                OutlinedTextField(
                    value = endTime,
                    onValueChange = { endTime = it },
                    label = { Text("End") },
                    modifier = Modifier.weight(1f),
                )
            }
        }
        Spacer(Modifier.height(6.dp))
        OutlinedTextField(
            value = location,
            onValueChange = { if (it.length <= 300) location = it },
            label = { Text("Location (optional)") },
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(6.dp))
        OutlinedTextField(
            value = notes,
            onValueChange = { if (it.length <= 4000) notes = it },
            label = { Text("Notes (optional)") },
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(10.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            PrimaryButton(
                text = if (busy) "Saving…" else "Save",
                enabled = !busy && title.isNotBlank(),
                onClick = {
                    val d = runCatching { LocalDate.parse(dateText.trim()) }.getOrDefault(defaultDate)
                    val time = if (allDay) {
                        EventTime.AllDay(d.toString(), d.plusDays(1).toString())
                    } else {
                        EventTime.Timed(
                            start = "${d}T${startTime.trim()}:00Z",
                            end = "${d}T${endTime.trim()}:00Z",
                            tz = "Europe/Lisbon",
                        )
                    }
                    onSave(
                        EventDraft(
                            title = title.trim(),
                            time = time,
                            location = location.trim().ifEmpty { null },
                            notes = notes.trim().ifEmpty { null },
                            repeat = repeat,
                        ),
                    )
                },
            )
            SecondaryButton(text = "Cancel", onClick = onDismiss)
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun EventDetailSheet(
    event: Event,
    sourceName: String,
    busy: Boolean,
    onTogglePrepare: () -> Unit,
    onDelete: () -> Unit,
    onDismiss: () -> Unit,
) {
    FlatCard {
        Text(event.title, color = SailColors.Text, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(4.dp))
        val whenLabel = when (val t = event.time) {
            is EventTime.AllDay -> "All day: ${t.startDate}"
            is EventTime.Timed -> "${t.start.take(10)} ${t.start.drop(11).take(5)}–${t.end.drop(11).take(5)} (${t.tz})"
        }
        Text("$sourceName · $whenLabel", color = SailColors.TextMuted, fontSize = 13.sp)
        if (!event.writable) {
            Text("Read-only feed event", color = SailColors.Warning, fontSize = 12.sp)
        }
        event.location?.let { loc ->
            Spacer(Modifier.height(4.dp))
            Text("Location: $loc", color = SailColors.TextMuted, fontSize = 13.sp)
        }
        event.notes?.let { n ->
            Spacer(Modifier.height(4.dp))
            Text(n, color = SailColors.TextMuted, fontSize = 13.sp)
        }
        Spacer(Modifier.height(10.dp))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            PrimaryButton(
                text = if (event.prepare) "Needs preparation: On" else "Needs preparation: Off",
                enabled = !busy,
                onClick = onTogglePrepare,
            )
            if (event.writable) {
                SecondaryButton(text = "Delete", enabled = !busy, onClick = onDelete)
            }
            SecondaryButton(text = "Close", onClick = onDismiss)
        }
    }
}
