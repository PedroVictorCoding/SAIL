package dev.sail.app.feature.arena

import dev.sail.app.core.UiState
import dev.sail.app.core.net.ApiResult
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class ArenaViewModelTest {
    private val dispatcher = StandardTestDispatcher()
    @Before fun setUp() { Dispatchers.setMain(dispatcher) }
    @After fun tearDown() { Dispatchers.resetMain() }
    private fun session(vm: ArenaPlayerViewModel) = (vm.state.value as UiState.Ready<PlayerSession>).data

    @Test fun catalogMapsReadyEmptyAndError() = runTest(dispatcher) {
        val service = FakeArenaService()
        val vm = ArenaViewModel(service)
        vm.loadCatalog(); advanceUntilIdle()
        assertTrue(vm.catalog.value is UiState.Ready)
        service.catalogResult = ApiResult.Ok(Catalog(emptyList()))
        vm.loadCatalog(); advanceUntilIdle()
        assertTrue(vm.catalog.value is UiState.Empty)
        service.catalogResult = ApiResult.Err(401, "Token refused")
        vm.loadCatalog(); advanceUntilIdle()
        assertEquals(UiState.Unavailable("Token refused"), vm.catalog.value)
    }

    @Test fun missingCurrentStepIsUnavailableInsteadOfGuessing() = runTest(dispatcher) {
        val service = FakeArenaService()
        val view = ArenaFixtures.read<LessonView>("GET /lessons/:id")
        service.lessonResult = ApiResult.Ok(view.copy(progress = view.progress.copy(currentStepId = "missing")))
        val vm = ArenaPlayerViewModel(service)
        vm.openLesson("lesson"); advanceUntilIdle()
        assertTrue(vm.state.value is UiState.Unavailable)
    }

    @Test fun lostResponseRetainsExactIdBodyAndBlocksNavigation() = runTest(dispatcher) {
        val service = FakeArenaService()
        service.attemptResult = ApiResult.Err(0, "Connection lost")
        var ids = 0
        val vm = ArenaPlayerViewModel(service) { "id-${++ids}" }
        vm.openLesson("lesson"); advanceUntilIdle()
        vm.submit(); advanceUntilIdle()
        val draft = vm.pending.value
        assertNotNull(draft)
        vm.openLesson("other"); advanceUntilIdle()
        assertEquals("lesson", session(vm).id)
        vm.update(AnswerDraft(text = "changed"))
        vm.submit(); advanceUntilIdle()
        assertEquals(service.attempts.first(), service.attempts.last())
        assertEquals(1, ids)
        service.attemptResult = ApiResult.Ok(ArenaFixtures.read("POST /lessons/:id/attempt (correct, hinted)"))
        vm.submit(); advanceUntilIdle()
        assertNull(vm.pending.value)
    }

    @Test fun recompositionDoesNotDiscardDraft() = runTest(dispatcher) {
        val service = FakeArenaService()
        val vm = ArenaPlayerViewModel(service)
        vm.openLesson("lesson"); advanceUntilIdle()
        vm.update(AnswerDraft(code = "retained draft"))
        vm.openLesson("lesson"); advanceUntilIdle()
        assertEquals("retained draft", session(vm).draft.code)
        assertEquals(1, service.lessonCalls)
    }

    @Test fun rateLimitRetainsSubmissionIdentity() = runTest(dispatcher) {
        val service = FakeArenaService()
        service.attemptResult = ApiResult.Err(429, "Slow down")
        val vm = ArenaPlayerViewModel(service)
        vm.openLesson("lesson"); advanceUntilIdle()
        vm.submit(); advanceUntilIdle()
        assertNotNull(vm.pending.value)
    }

    @Test fun validationRefusalLetsLearnerEditAgain() = runTest(dispatcher) {
        val service = FakeArenaService()
        service.attemptResult = ApiResult.Err(422, "Invalid answer")
        val vm = ArenaPlayerViewModel(service)
        vm.openLesson("lesson"); advanceUntilIdle()
        vm.submit(); advanceUntilIdle()
        assertNull(vm.pending.value)
        assertEquals("Invalid answer", vm.error.value)
    }

    @Test fun completedReportDoesNotStartAnotherDiagnostic() = runTest(dispatcher) {
        val service = FakeArenaService()
        val vm = ArenaPlayerViewModel(service)
        vm.openDiagnostic("rust-systems", DiagnosticInfo("definition", "runtime-session", "done"))
        advanceUntilIdle()
        assertEquals(0, service.diagnosticCalls)
        assertEquals(1, service.reportCalls)
        assertTrue(session(vm).finished)
        assertEquals("runtime-session", session(vm).id)
    }

    @Test fun diagnosticMovesOnlyToServerSuppliedNextStep() = runTest(dispatcher) {
        val service = FakeArenaService()
        val grade = ArenaFixtures.read<Grade>("POST /diagnostics/:id/attempt")
        service.attemptResult = ApiResult.Ok(grade.copy(result = "incorrect"))
        val vm = ArenaPlayerViewModel(service)
        vm.openDiagnostic("rust-systems", null); advanceUntilIdle()
        vm.update(AnswerDraft(choices = listOf("a"), confidence = "low"))
        vm.submit(); advanceUntilIdle()
        vm.continueActivity(); advanceUntilIdle()
        assertEquals(grade.next?.id, session(vm).step?.id)
        assertEquals(1, service.diagnosticCalls)
    }

    @Test fun diagnosticsHaveNoHints() = runTest(dispatcher) {
        val service = FakeArenaService()
        val vm = ArenaPlayerViewModel(service)
        vm.openDiagnostic("rust-systems", null); advanceUntilIdle()
        vm.hint(); advanceUntilIdle()
        assertEquals(0, service.hintCalls)
    }

    @Test fun unavailableCanContinueByReloadingCanonicalProgress() = runTest(dispatcher) {
        val service = FakeArenaService()
        service.attemptResult = ApiResult.Ok(ArenaFixtures.read("POST /lessons/:id/attempt (rust_edit, sandbox unavailable)"))
        val vm = ArenaPlayerViewModel(service)
        vm.openLesson("lesson"); advanceUntilIdle()
        vm.submit(); advanceUntilIdle()
        assertEquals("unavailable", session(vm).grade?.result)
        vm.continueActivity(); advanceUntilIdle()
        assertEquals(2, service.lessonCalls)
        assertNull(session(vm).grade)
    }

    @Test fun serverGateRemainsBlockedAndLessonsRemainReadable() = runTest(dispatcher) {
        val service = FakeArenaService()
        val view = ArenaFixtures.read<LessonView>("GET /lessons/:id")
        service.lessonResult = ApiResult.Ok(view.copy(progress = view.progress.copy(state = "completed")))
        val vm = ArenaPlayerViewModel(service)
        vm.openLesson("lesson"); advanceUntilIdle()
        vm.certify(); advanceUntilIdle()
        assertEquals("blocked_by_review", session(vm).gate?.decision)
        assertFalse(session(vm).gate?.certified ?: true)
        vm.browse(view.lesson.steps.last())
        assertTrue(session(vm).browsing)
        vm.submit(); advanceUntilIdle()
        assertTrue(service.attempts.isEmpty())
    }

    @Test fun emptyQueueRetainsCatchUpInformation() = runTest(dispatcher) {
        val vm = ArenaViewModel(FakeArenaService())
        vm.loadReviews("rust-systems"); advanceUntilIdle()
        val data = (vm.queue.value as UiState.Ready<ReviewQueue>).data
        assertTrue(data.items.isEmpty())
        assertEquals(5, data.catchUp.cap)
    }
}
