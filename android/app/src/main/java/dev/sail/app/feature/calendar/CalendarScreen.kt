package dev.sail.app.feature.calendar

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.sail.app.core.LocalAppContainer
import dev.sail.app.core.design.EmptyState
import dev.sail.app.core.design.FlatCard
import dev.sail.app.core.design.LoadingSkeleton
import dev.sail.app.core.design.PrimaryButton
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.design.ScreenGutter
import dev.sail.app.core.design.SecondaryButton
import dev.sail.app.core.sailViewModel
import java.time.LocalDate
import java.time.format.TextStyle
import java.util.Locale

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun CalendarScreen(
    onBack: () -> Unit,
) {
    val container = LocalAppContainer.current
    val vm: CalendarViewModel = sailViewModel(key = "calendar") { CalendarViewModel(container.api) }
    val state by vm.state.collectAsState()

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(SailColors.Base)
            .padding(horizontal = ScreenGutter, vertical = 14.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            SecondaryButton(text = "Back", onClick = onBack)
            Text("Calendar", color = SailColors.Text, fontSize = 18.sp, fontWeight = FontWeight.SemiBold)
            PrimaryButton(text = "Add", onClick = vm::openCreate)
        }

        Spacer(Modifier.height(10.dp))

        // Compact month strip (7-day week around selectedDate)
        val weekStart = state.selectedDate.minusDays((state.selectedDate.dayOfWeek.value - 1).toLong())
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            (0L..6L).forEach { offset ->
                val day = weekStart.plusDays(offset)
                val selected = day == state.selectedDate
                Column(
                    modifier = Modifier
                        .weight(1f)
                        .padding(horizontal = 2.dp)
                        .background(
                            if (selected) Color(0x40496F9D) else Color(0x14FFFFFF),
                            RoundedCornerShape(10.dp),
                        )
                        .border(1.dp, if (selected) SailColors.Text else SailColors.Line, RoundedCornerShape(10.dp))
                        .clickable { vm.selectDate(day) }
                        .padding(vertical = 6.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Text(
                        day.dayOfWeek.getDisplayName(TextStyle.SHORT, Locale.ENGLISH).take(2),
                        color = SailColors.TextMuted,
                        fontSize = 11.sp,
                    )
                    Text(
                        day.dayOfMonth.toString(),
                        color = SailColors.Text,
                        fontSize = 13.sp,
                        fontWeight = if (selected) FontWeight.Bold else FontWeight.Normal,
                    )
                }
            }
        }

        Spacer(Modifier.height(8.dp))

        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            SecondaryButton(text = if (state.dayView) "Agenda" else "Day", onClick = { vm.toggleDayView(!state.dayView) })
            SecondaryButton(text = "Today", onClick = { vm.selectDate(LocalDate.now()) })
            SecondaryButton(text = "Sources", onClick = vm::toggleSources)
        }

        state.error?.let { err ->
            Spacer(Modifier.height(6.dp))
            Text(err, color = SailColors.Error, fontSize = 13.sp)
        }
        state.banner?.let { msg ->
            Spacer(Modifier.height(6.dp))
            Text(msg, color = SailColors.Good, fontSize = 13.sp)
        }

        Spacer(Modifier.height(8.dp))

        if (state.showCreateSheet) {
            CreateEventSheet(
                defaultDate = state.selectedDate,
                busy = state.busy,
                onSave = vm::createEvent,
                onDismiss = vm::closeSheets,
            )
            Spacer(Modifier.height(8.dp))
        }

        state.selectedEvent?.let { ev ->
            val srcName = state.sources.find { it.id == ev.sourceId }?.name ?: "Calendar"
            EventDetailSheet(
                event = ev,
                sourceName = srcName,
                busy = state.busy,
                onTogglePrepare = { vm.togglePrepare(ev) },
                onDelete = { vm.deleteEvent(ev.id) },
                onDismiss = vm::closeSheets,
            )
            Spacer(Modifier.height(8.dp))
        }

        if (state.showSources) {
            SourcesCard(
                sources = state.sources,
                busy = state.busy,
                onAddFeed = vm::addFeed,
                onSync = vm::syncSource,
                onRemove = vm::removeSource,
            )
            Spacer(Modifier.height(8.dp))
        }

        if (state.loading && state.window.occurrences.isEmpty()) {
            LoadingSkeleton(lines = 5)
            return
        }

        val visibleOccurrences = if (state.dayView) {
            val dayStr = state.selectedDate.toString()
            state.window.occurrences.filter { occ ->
                when (val t = occ.time) {
                    is EventTime.AllDay -> t.startDate <= dayStr && dayStr < t.endDate
                    is EventTime.Timed -> t.start.startsWith(dayStr)
                }
            }
        } else {
            state.window.occurrences
        }.filter { it.status != EventStatus.CANCELLED }

        if (visibleOccurrences.isEmpty()) {
            EmptyState(title = "No events this week.", detail = "Tap Add to schedule something, or open Sources to subscribe to an .ics feed.")
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(visibleOccurrences, key = { it.key }) { occ ->
                    val src = state.sources.find { it.id == occ.sourceId }
                    OccurrenceRow(
                        occ = occ,
                        sourceName = src?.name ?: "Calendar",
                        sourceColorHex = src?.color ?: "#496f9d",
                        onClick = { vm.openEvent(occ.eventId) },
                    )
                }
            }
        }
    }
}

@Composable
private fun OccurrenceRow(
    occ: Occurrence,
    sourceName: String,
    sourceColorHex: String,
    onClick: () -> Unit,
) {
    val barColor = runCatching { Color(android.graphics.Color.parseColor(sourceColorHex)) }
        .getOrDefault(SailColors.Thinking)
    FlatCard {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .clickable(onClick = onClick),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                modifier = Modifier
                    .width(4.dp)
                    .height(36.dp)
                    .background(barColor, RoundedCornerShape(2.dp)),
            )
            Spacer(Modifier.width(10.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = occ.title,
                    color = SailColors.Text,
                    fontSize = 15.sp,
                    fontWeight = FontWeight.Medium,
                    textDecoration = if (occ.status == EventStatus.CANCELLED) TextDecoration.LineThrough else null,
                )
                val meta = listOfNotNull(
                    formatOccurrenceTime(occ),
                    sourceName,
                    occ.location,
                    if (occ.prepare) "Prepare" else null,
                ).joinToString(" · ")
                Text(meta, color = SailColors.TextMuted, fontSize = 12.sp)
            }
        }
    }
}

@Composable
private fun SourcesCard(
    sources: List<Source>,
    busy: Boolean,
    onAddFeed: (String, String) -> Unit,
    onSync: (String) -> Unit,
    onRemove: (String) -> Unit,
) {
    var name by remember { mutableStateOf("") }
    var url by remember { mutableStateOf("") }

    FlatCard {
        Text("Calendar sources", color = SailColors.Text, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(6.dp))
        OutlinedTextField(
            value = name,
            onValueChange = { name = it },
            label = { Text("Feed name") },
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(4.dp))
        OutlinedTextField(
            value = url,
            onValueChange = { url = it },
            label = { Text("https:// or webcal:// .ics URL") },
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(6.dp))
        PrimaryButton(
            text = "Add a calendar feed",
            enabled = !busy && name.isNotBlank() && url.isNotBlank(),
            onClick = {
                val n = name
                val u = url
                name = ""
                url = ""
                onAddFeed(n, u)
            },
        )
        Spacer(Modifier.height(8.dp))
        sources.forEach { src ->
            Row(
                modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text("${src.name} (${src.eventCount})", color = SailColors.Text, fontSize = 13.sp)
                    Text(src.lastError ?: src.freshness.name.lowercase(), color = SailColors.TextMuted, fontSize = 11.sp)
                }
                if (src.kind == SourceKind.ICS_FEED) {
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        SecondaryButton("Sync", onClick = { onSync(src.id) }, enabled = !busy)
                        SecondaryButton("Remove", onClick = { onRemove(src.id) }, enabled = !busy)
                    }
                }
            }
        }
    }
}
