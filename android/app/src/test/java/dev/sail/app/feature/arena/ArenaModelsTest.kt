package dev.sail.app.feature.arena

import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ArenaModelsTest {
    @Test fun catalogReadsReadyAndPlannedWithoutInventedProgress() {
        val catalog = ArenaFixtures.read<Catalog>("GET /courses")
        assertEquals(3, catalog.courses.size)
        assertEquals(2, catalog.courses.count { it.status == "ready" })
        assertNull(catalog.courses.last().progress)
    }

    @Test fun detailUsesRuntimeDiagnosticSessionId() {
        val detail = ArenaFixtures.read<CourseDetail>("GET /courses/rust-systems")
        assertTrue(detail.chapters.isNotEmpty())
        assertEquals("not_started", detail.diagnostic?.state)
        assertNull(detail.diagnostic?.sessionId)
    }

    @Test fun realLessonKeepsAllStepKindsAndServerProgress() {
        val lesson = ArenaFixtures.read<LessonView>("GET /lessons/:id")
        assertEquals("intro", lesson.progress.currentStepId)
        assertTrue(lesson.lesson.steps.any { it.kind == "fill" })
        assertTrue(lesson.lesson.steps.any { it.multi })
    }

    @Test fun assistanceOverridesProvisionalProjection() {
        val grade = ArenaFixtures.read<Grade>("POST /lessons/:id/attempt (correct, hinted)")
        assertEquals("provisional", grade.evidence)
        assertEquals("Assisted · not an independent pass", grade.evidenceLabel())
    }

    @Test fun incorrectProjectionDoesNotClaimIndependentSuccess() {
        val grade = ArenaFixtures.read<Grade>("POST /lessons/:id/attempt (incorrect)")
        assertEquals("No independent pass on this attempt", grade.evidenceLabel())
    }

    @Test fun unavailableIsNeitherRightNorWrong() {
        val grade = ArenaFixtures.read<Grade>("POST /lessons/:id/attempt (rust_edit, sandbox unavailable)")
        assertEquals("Not assessed", grade.evidenceLabel())
        assertEquals("continue", grade.gateDecision)
        assertTrue(grade.feedback.unavailableReason.orEmpty().isNotEmpty())
    }

    @Test fun reviewQueueKeepsCatchUpWhenEmpty() {
        val queue = ArenaFixtures.read<ReviewQueue>("GET /reviews/due")
        assertTrue(queue.items.isEmpty())
        assertEquals(5, queue.catchUp.cap)
        assertEquals(0, queue.catchUp.totalDue)
    }

    @Test fun evidenceKeepsExplanationAndUnassessedState() {
        val evidence = ArenaFixtures.read<EvidenceList>("GET /skills/evidence")
        assertEquals(4, evidence.objectives.size)
        assertTrue(evidence.objectives.any { it.notAssessed })
        assertTrue(evidence.objectives.all { it.why.isNotBlank() })
    }

    @Test fun answerPayloadContainsOnlyAnswerAndClientIdentity() {
        val view = ArenaFixtures.read<LessonView>("GET /lessons/:id")
        val step = view.lesson.steps.first { it.kind == "fill" }
        val answer = AnswerDraft(text = "<img src=x>").answer(step)
        val request = AttemptRequest("same-id", step.id, answer)
        val wire = ArenaFixtures.json.parseToJsonElement(ArenaFixtures.json.encodeToString(request)).jsonObject
        assertEquals(setOf("attempt_id", "step_id", "answer"), wire.keys)
        assertEquals(JsonPrimitive("<img src=x>"), wire.getValue("answer").jsonObject["text"])
    }

    @Test fun orderingPreservesEveryItemAtBoundaries() {
        assertEquals(listOf("b", "a", "c"), moveAnswer(listOf("a", "b", "c"), 1, -1))
        assertEquals(listOf("a", "b"), moveAnswer(listOf("a", "b"), 0, -1))
    }

    @Test fun rustCodeAndOrderedAnswersArePlainPayloads() {
        val rust = Step("edit", "rust_edit", "Edit", emptyList(), 2, false)
        val source = "fn main() { println!(\"<script>\"); }"
        assertEquals(JsonPrimitive(source), AnswerDraft(code = source).answer(rust)?.get("code"))
        val order = Step("order", "order", "Arrange", emptyList(), 2, false,
            items = listOf(Option("a", "First"), Option("b", "Second")))
        assertTrue(AnswerDraft(order = listOf("b", "a")).ready(order))
        assertFalse(AnswerDraft(order = listOf("a", "a")).ready(order))
    }

    @Test fun unknownStepCannotBeSubmitted() {
        val step = Step("future", "future", "Future step", emptyList(), 0, false)
        assertFalse(AnswerDraft().ready(step))
    }

    @Test fun confidenceIsRequiredOnlyWhenServerRequestsIt() {
        val step = Step("choice", "choice", "Choose", emptyList(), 0, true)
        assertFalse(AnswerDraft(choices = listOf("a")).ready(step))
        assertTrue(AnswerDraft(choices = listOf("a"), confidence = "low").ready(step))
    }
}
