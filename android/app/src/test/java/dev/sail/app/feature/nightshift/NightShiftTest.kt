package dev.sail.app.feature.nightshift

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

class NightShiftTest {
    private fun loadFixture(name: String): String {
        val stream = requireNotNull(javaClass.classLoader?.getResourceAsStream("fixtures/nightshift/$name")) {
            "Missing fixture: $name"
        }
        return stream.bufferedReader().use { it.readText() }
    }

    @Test
    fun allTenGetFixturesDeserializeCleanly() {
        val getFixtures = listOf(
            "empty.json",
            "shaping.json",
            "proposed.json",
            "unshapeable.json",
            "queued_with_plan.json",
            "working.json",
            "morning_mixed.json",
            "shipped.json",
            "external_used.json",
            "mesh_resting.json",
            "morning_issue_and_halt.json",
        )
        for (name in getFixtures) {
            val parsed = NightShiftJson.decodeFromString(NightShiftResponse.serializer(), loadFixture(name))
            assertNotNull(parsed.settings)
        }

        val mixed = NightShiftJson.decodeFromString(NightShiftResponse.serializer(), loadFixture("morning_mixed.json"))
        assertEquals(3, mixed.errands.size)
        assertTrue(mixed.errands[0].state is ErrandState.Ready)
        assertTrue(mixed.errands[1].state is ErrandState.NeedsYou)
        assertTrue(mixed.errands[2].state is ErrandState.Failed)
        assertNotNull(mixed.morning)
        assertEquals(4, mixed.morning?.items?.size)

        val external = NightShiftJson.decodeFromString(NightShiftResponse.serializer(), loadFixture("external_used.json"))
        assertTrue(external.errands[0].externalUsed)
        assertEquals(RanOnKind.EXTERNAL, (external.errands[0].state as ErrandState.Ready).result.ranOn.kind)

        val resting = NightShiftJson.decodeFromString(NightShiftResponse.serializer(), loadFixture("mesh_resting.json"))
        assertTrue(resting.mesh?.resting == true)
    }

    @Test
    fun wsFixturesDeserializeAndApplyToResponse() {
        val wsErrand = NightShiftJson.decodeFromString(WsNightShiftErrand.serializer(), loadFixture("ws_errand.json"))
        assertEquals("nightshift", wsErrand.type)
        assertTrue(wsErrand.errand.state is ErrandState.Working)

        val wsMorning = NightShiftJson.decodeFromString(WsNightShiftMorning.serializer(), loadFixture("ws_morning.json"))
        assertEquals("nightshift_morning", wsMorning.type)
        assertEquals(2, wsMorning.report.items.size)

        val base = NightShiftJson.decodeFromString(NightShiftResponse.serializer(), loadFixture("empty.json"))
        val updated = upsertErrandInResponse(base, wsErrand.errand)
        assertEquals(1, updated.errands.size)
        assertEquals("e-0a1b2c3d4e5f", updated.errands[0].id)
    }

    @Test
    fun diffParserCountsAndCollapsesOver400Lines() {
        val smallLines = buildString {
            appendLine("diff --git a/a.kt b/a.kt")
            appendLine("--- a/a.kt")
            appendLine("+++ b/a.kt")
            appendLine("@@ -1,2 +1,3 @@")
            appendLine("+added1")
            appendLine("-removed1")
            appendLine(" context")
        }
        val largeLines = buildString {
            appendLine("diff --git a/big.kt b/big.kt")
            appendLine("--- a/big.kt")
            appendLine("+++ b/big.kt")
            repeat(405) { appendLine("+line $it") }
        }
        val parsed = parseUnifiedDiff(smallLines + largeLines)
        assertEquals(2, parsed.size)
        assertEquals("a.kt", parsed[0].path)
        assertEquals(1, parsed[0].added)
        assertEquals(1, parsed[0].removed)
        assertFalse(parsed[0].collapsedByDefault)

        assertEquals("big.kt", parsed[1].path)
        assertEquals(405, parsed[1].added)
        assertTrue(parsed[1].collapsedByDefault)
    }

    @Test
    fun morningIssueAndHaltParsesHaltedAndGitHubPrLinkOnly() {
        val report = NightShiftJson.decodeFromString(
            NightShiftResponse.serializer(),
            loadFixture("morning_issue_and_halt.json"),
        ).morning
        assertNotNull(report)
        assertEquals(
            "The night stopped early: the model server stopped responding at 03:12.",
            report?.halted,
        )
        val items = report!!.items
        assertEquals(3, items.size)
        assertEquals(
            "https://github.com/PedroVictorCoding/SAIL/pull/501",
            parseIssueDraftPrUrl(items[0]),
        )
        assertEquals(null, parseIssueDraftPrUrl(items[1]))
        assertEquals(null, parseIssueDraftPrUrl(items[2]))

        // Non-github URL or errand with same prefix must NOT become a link
        val evilIssue = items[0].copy(line = "Draft PR opened: https://evil.example/pr/1")
        assertEquals(null, parseIssueDraftPrUrl(evilIssue))
        val errandWithPrefix = items[2].copy(line = "Draft PR opened: https://github.com/PedroVictorCoding/SAIL/pull/501")
        assertEquals(null, parseIssueDraftPrUrl(errandWithPrefix))
    }
}
