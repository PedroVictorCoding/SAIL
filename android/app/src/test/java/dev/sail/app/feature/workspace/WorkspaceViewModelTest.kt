package dev.sail.app.feature.workspace

import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import kotlinx.serialization.decodeFromString
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class WorkspaceViewModelTest {
    private val dispatcher = StandardTestDispatcher()
    private val json = SailApi.defaultJson

    @Before
    fun setUp() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private fun fixture(path: String): String =
        requireNotNull(javaClass.getResource(path)) { "missing test fixture $path" }.readText()

    private fun health() = json.decodeFromString<Health>(fixture("/fixtures/workspace/health.json"))

    private fun mind() = json.decodeFromString<MindStatus>(fixture("/fixtures/workspace/mind.json"))

    private fun metrics() = json.decodeFromString<SystemMetrics>(fixture("/fixtures/workspace/metrics.json"))

    private fun catalog() = json.decodeFromString<ModelCatalog>(fixture("/fixtures/workspace/catalog.json"))

    private fun plans() = json.decodeFromString<PlansResponse>(fixture("/fixtures/workspace/plans.json"))

    private class FakeSource(
        private val healthResult: ApiResult<Health>,
        private val mindResult: ApiResult<MindStatus>,
        private val metricsResult: ApiResult<SystemMetrics>,
        private val catalogResult: ApiResult<ModelCatalog>,
        private val plansResult: ApiResult<PlansResponse>,
    ) : WorkspaceSource {
        override suspend fun health(): ApiResult<Health> = healthResult

        override suspend fun mind(): ApiResult<MindStatus> = mindResult

        override suspend fun metrics(): ApiResult<SystemMetrics> = metricsResult

        override suspend fun models(): ApiResult<ModelCatalog> = catalogResult

        override suspend fun plans(): ApiResult<PlansResponse> = plansResult
    }

    @Test
    fun `refresh loads every live status source`() = runTest(dispatcher) {
        val vm = WorkspaceViewModel(
            FakeSource(
                healthResult = ApiResult.Ok(health()),
                mindResult = ApiResult.Ok(mind()),
                metricsResult = ApiResult.Ok(metrics()),
                catalogResult = ApiResult.Ok(catalog()),
                plansResult = ApiResult.Ok(plans()),
            ),
        )

        vm.refresh()
        advanceUntilIdle()

        val state = vm.state.value
        assertFalse(state.loading)
        assertNotNull(state.updatedAt)
        assertTrue(state.health.valueOrNull() != null)
        assertTrue(state.mind.valueOrNull() != null)
        assertTrue(state.metrics.valueOrNull() != null)
        assertTrue(state.catalog.valueOrNull() != null)
        assertTrue(state.plans.valueOrNull() != null)
        assertEquals(LiveState.Active, buildSubsystems(state).first { it.id == "core" }.state)
    }

    @Test
    fun `failed sources stay unavailable without hiding the others`() = runTest(dispatcher) {
        val vm = WorkspaceViewModel(
            FakeSource(
                healthResult = ApiResult.Err(0, "SAIL is unreachable"),
                mindResult = ApiResult.Err(0, "SAIL is unreachable"),
                metricsResult = ApiResult.Err(503, "Metrics are unavailable"),
                catalogResult = ApiResult.Err(0, "SAIL is unreachable"),
                plansResult = ApiResult.Err(404, "No plans route"),
            ),
        )

        vm.refresh()
        advanceUntilIdle()

        val state = vm.state.value
        assertFalse(state.loading)
        assertFalse(state.isEmpty)
        val subsystems = buildSubsystems(state).associateBy { it.id }
        assertEquals(LiveState.Offline, subsystems.getValue("core").state)
        assertEquals(LiveState.Offline, subsystems.getValue("system").state)
        assertEquals(LiveState.Offline, subsystems.getValue("forge").state)
        assertEquals("SAIL is unreachable", workspaceError(state))
    }

    @Test
    fun `start polls once immediately and stop cancels the loop`() = runTest(dispatcher) {
        val vm = WorkspaceViewModel(
            FakeSource(
                healthResult = ApiResult.Ok(health()),
                mindResult = ApiResult.Ok(mind()),
                metricsResult = ApiResult.Ok(metrics()),
                catalogResult = ApiResult.Ok(catalog()),
                plansResult = ApiResult.Ok(plans()),
            ),
        )

        vm.start()
        runCurrent()
        assertFalse(vm.state.value.loading)
        assertNotNull(vm.state.value.updatedAt)

        vm.stop()
        runCurrent()
        assertNotNull(vm.state.value.updatedAt)
    }
}
