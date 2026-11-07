package dev.sail.app.feature.workspace

import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class WorkspaceModelsTest {
    private val json = SailApi.defaultJson

    private fun fixture(path: String): String =
        requireNotNull(javaClass.getResource(path)) { "missing test fixture $path" }.readText()

    private fun health() = json.decodeFromString<Health>(fixture("/fixtures/workspace/health.json"))

    private fun mind() = json.decodeFromString<MindStatus>(fixture("/fixtures/workspace/mind.json"))

    private fun activeData(): WorkspaceData = WorkspaceData(
        health = ApiResult.Ok(health()),
        mind = ApiResult.Ok(mind()),
        metrics = ApiResult.Ok(
            json.decodeFromString<SystemMetrics>(fixture("/fixtures/workspace/metrics.json")),
        ),
        catalog = ApiResult.Ok(
            json.decodeFromString<ModelCatalog>(fixture("/fixtures/workspace/catalog.json")),
        ),
        plans = ApiResult.Ok(
            json.decodeFromString<PlansResponse>(fixture("/fixtures/workspace/plans.json")),
        ),
    )

    @Test
    fun `decodes real shape workspace responses`() {
        val parsedHealth = health()
        assertEquals(2, parsedHealth.speech?.readyCount)
        assertTrue(parsedHealth.llm?.ready == true)
        assertEquals(16000, parsedHealth.sampleRate)

        val parsedMind = mind()
        assertEquals(1, parsedMind.models.size)
        assertEquals("ready", parsedMind.models.single().state)
        assertEquals(2, parsedMind.metrics?.queued)
        assertEquals(1, parsedMind.limits?.maxParallel)

        val metrics = json.decodeFromString<SystemMetrics>(fixture("/fixtures/workspace/metrics.json"))
        assertEquals(12.4, metrics.cpuPercent ?: 0.0, 0.001)
        assertEquals(1, metrics.disks.size)
        assertEquals(35.0, metrics.gpus.single().busyPercent ?: 0.0, 0.001)

        val catalog = json.decodeFromString<ModelCatalog>(fixture("/fixtures/workspace/catalog.json"))
        assertEquals("qwen2.5-7b", catalog.running)
        assertTrue(catalog.models.single().installed)

        val plans = json.decodeFromString<PlansResponse>(fixture("/fixtures/workspace/plans.json"))
        assertEquals("running", plans.plans.single().plan.state)
    }

    @Test
    fun `derives subsystem states from live endpoints`() {
        val subsystems = buildSubsystems(activeData()).associateBy { it.id }

        assertEquals(LiveState.Active, subsystems.getValue("core").state)
        assertEquals(LiveState.Active, subsystems.getValue("mind").state)
        assertEquals(LiveState.Active, subsystems.getValue("system").state)
        assertEquals(LiveState.Active, subsystems.getValue("forge").state)
        assertEquals(LiveState.NoStatus, subsystems.getValue("memory").state)
        assertEquals(LiveState.NoStatus, subsystems.getValue("flow").state)
        assertEquals(LiveState.NoStatus, subsystems.getValue("arena").state)
        assertTrue(subsystems.getValue("mind").detail.contains("1/1 models ready"))
    }

    @Test
    fun `model errors and waiting plans pull attention`() {
        val base = activeData()
        val currentMind = base.mind.valueOrNull() ?: error("the fixture must decode")
        val withError = currentMind.copy(
            models = listOf(
                currentMind.models.single().copy(lastError = JsonPrimitive("backend unavailable")),
            ),
        )
        val subsystems = buildSubsystems(
            base.copy(
                mind = ApiResult.Ok(withError),
                plans = ApiResult.Ok(PlansResponse()),
            ),
        ).associateBy { it.id }

        assertEquals(LiveState.Attention, subsystems.getValue("mind").state)
        assertEquals(LiveState.Idle, subsystems.getValue("forge").state)

        val awaiting = base.copy(
            plans = ApiResult.Ok(
                PlansResponse(
                    plans = listOf(
                        PlanView(plan = Plan(id = "p2", goal = "Wait", state = "awaiting_approval")),
                    ),
                ),
            ),
        )
        assertEquals(
            LiveState.Attention,
            buildSubsystems(awaiting).first { it.id == "forge" }.state,
        )
    }

    @Test
    fun `core reports attention when the language model is not ready`() {
        val base = activeData()
        val degraded = health().copy(llm = health().llm?.copy(ready = false))
        val subsystems = buildSubsystems(base.copy(health = ApiResult.Ok(degraded))).associateBy { it.id }
        assertEquals(LiveState.Attention, subsystems.getValue("core").state)
    }

    @Test
    fun `missing endpoints are offline not guessed`() {
        val subsystems = buildSubsystems(WorkspaceData()).associateBy { it.id }
        assertEquals(LiveState.Offline, subsystems.getValue("core").state)
        assertEquals(LiveState.Offline, subsystems.getValue("mind").state)
        assertEquals(LiveState.Offline, subsystems.getValue("system").state)
        assertEquals(LiveState.Offline, subsystems.getValue("forge").state)
        assertEquals(LiveState.NoStatus, subsystems.getValue("memory").state)
        assertTrue(subsystems.getValue("core").detail.contains("did not answer"))
    }

    @Test
    fun `topology combines both endpoint states`() {
        val subsystems = buildSubsystems(activeData()).associateBy { it.id }
        val links = buildTopology(subsystems.values.toList()).associateBy { "${it.from}:${it.to}" }

        assertEquals(LiveState.Active, links.getValue("core:mind").state)
        assertEquals(LiveState.NoStatus, links.getValue("core:memory").state)
        assertEquals("active · no status", links.getValue("core:memory").detail)
    }

    @Test
    fun `reports the first workspace error`() {
        assertNull(workspaceError(activeData()))
        assertEquals(
            "SAIL is unreachable",
            workspaceError(WorkspaceData(health = ApiResult.Err(0, "SAIL is unreachable"))),
        )
    }

    @Test
    fun `formats host measurements honestly`() {
        assertEquals("16.0 GB", formatBytes(17179869184))
        assertEquals("8.0 GB", formatBytes(8589934592))
        assertEquals("4.0 GB", formatBytes(4294967296))
        assertEquals("—", formatBytes(null))
        assertEquals("42%", formatPercent(42.4))
        assertEquals("—", formatPercent(null))
        assertEquals("0.42", formatDecimal(0.42))
        assertEquals("1d 1h", formatUptime(90061.0))
        assertEquals("15h 0m", formatUptime(54000.0))
        assertEquals("3m", formatUptime(180.0))
        assertEquals("qwen2.5-7b-instruct", shortModel("/models/qwen2.5-7b-instruct.gguf"))
        assertEquals("Awaiting approval", humanPlanState("awaiting_approval"))
    }
}
