package dev.sail.app.feature.calendar

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CalendarTest {
    private fun loadFixture(name: String): String {
        val stream = requireNotNull(javaClass.classLoader?.getResourceAsStream("fixtures/calendar/$name")) {
            "Missing fixture: $name"
        }
        return stream.bufferedReader().use { it.readText() }
    }

    @Test
    fun allFifteenCalendarFixturesDeserializeCleanly() {
        val firstRun = CalendarJson.decodeFromString(SourcesListResponse.serializer(), loadFixture("sources_first_run.json"))
        assertEquals(1, firstRun.sources.size)
        assertEquals("src_local000000", firstRun.sources[0].id)

        val mixedSources = CalendarJson.decodeFromString(SourcesListResponse.serializer(), loadFixture("sources_mixed.json"))
        assertEquals(4, mixedSources.sources.size)
        assertEquals(Freshness.STALE, mixedSources.sources[2].freshness)
        assertEquals(Freshness.FAILED, mixedSources.sources[3].freshness)

        val weekMixed = CalendarJson.decodeFromString(EventsWindow.serializer(), loadFixture("week_mixed.json"))
        assertEquals(6, weekMixed.occurrences.size)
        assertFalse(weekMixed.truncated)

        val weekEmpty = CalendarJson.decodeFromString(EventsWindow.serializer(), loadFixture("week_empty.json"))
        assertTrue(weekEmpty.occurrences.isEmpty())

        val monthDense = CalendarJson.decodeFromString(EventsWindow.serializer(), loadFixture("month_dense.json"))
        assertEquals(7, monthDense.occurrences.size)

        val truncated = CalendarJson.decodeFromString(EventsWindow.serializer(), loadFixture("window_truncated.json"))
        assertTrue(truncated.truncated)

        val evLocal = CalendarJson.decodeFromString(Event.serializer(), loadFixture("event_local.json"))
        assertTrue(evLocal.writable)

        val evFeed = CalendarJson.decodeFromString(Event.serializer(), loadFixture("event_feed_readonly.json"))
        assertFalse(evFeed.writable)

        val evRec = CalendarJson.decodeFromString(Event.serializer(), loadFixture("event_recurring.json"))
        assertEquals(RepeatPreset.WEEKLY, evRec.repeat)

        val preview = CalendarJson.decodeFromString(ImportPreview.serializer(), loadFixture("import_preview.json"))
        assertEquals(3, preview.events.size)
        assertEquals(1, preview.skipped.size)

        val result = CalendarJson.decodeFromString(ImportResult.serializer(), loadFixture("import_result.json"))
        assertEquals(2, result.imported)

        val prefs = CalendarJson.decodeFromString(Preferences.serializer(), loadFixture("preferences.json"))
        assertEquals("Europe/Lisbon", prefs.timeZone)

        val errBlocked = CalendarJson.decodeFromString(CalendarErrorBody.serializer(), loadFixture("error_blocked_address.json"))
        assertEquals("blocked_address", errBlocked.error.code)

        val errRo = CalendarJson.decodeFromString(CalendarErrorBody.serializer(), loadFixture("error_read_only.json"))
        assertEquals("read_only", errRo.error.code)

        val ws = CalendarJson.decodeFromString(WsCalendarChanged.serializer(), loadFixture("ws_calendar_changed.json"))
        assertEquals(42L, ws.revision)
    }

    @Test
    fun nextUpTakesAtMostThreeNonCancelledOccurrences() {
        val week = CalendarJson.decodeFromString(EventsWindow.serializer(), loadFixture("week_mixed.json"))
        val top3 = nextUpOccurrences(week.occurrences, limit = 3)
        assertEquals(3, top3.size)
        assertEquals("Algorithms lecture", top3[0].title)
        assertEquals("09:00–10:30", formatOccurrenceTime(top3[0]))
    }
}
