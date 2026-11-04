// Run with node --test crates/sail-dialogue/tests/learn_ui_test.cjs
const {test} = require('node:test');
const assert = require('node:assert/strict');
const ui = require('../web/learn.js');

test('SSE parsing keeps partial frames for the next chunk', () => {
  const first = ui.learnParseSse('event: view\ndata: {"a":1}\n\n: keep-alive\n\nevent: view\ndata: {"b"');
  assert.deepEqual(first.events, [{event: 'view', data: '{"a":1}'}]);
  const second = ui.learnParseSse(first.rest + ':2}\r\n\r\n');
  assert.deepEqual(second.events, [{event: 'view', data: '{"b":2}'}]);
  assert.equal(second.rest, '');
});

test('map steps follow teaching order and carry text labels', () => {
  const view = {plan: {nodes: [{id: 'c', title: 'C'}, {id: 'a', title: 'A'}, {id: 'b', title: 'B'}], order: ['a', 'b', 'c']},
    progress: {current: 'b', visited: ['a', 'b']}};
  assert.deepEqual(ui.learnStepStates(view).map(s => [s.id, s.state, s.label]),
    [['a', 'visited', 'Covered'], ['b', 'current', 'Now'], ['c', 'upcoming', 'Next']]);
  assert.deepEqual(ui.learnStepStates({plan: null}), []);
});

test('status reflects paused, running, failure and waiting states', () => {
  assert.equal(ui.learnStatus({paused: true, running: true}).kind, 'paused');
  assert.equal(ui.learnStatus({running: true, mode: 'assess'}).kind, 'running');
  const failed = ui.learnStatus({last_failure: 'model offline'});
  assert.equal(failed.kind, 'failed'); assert.equal(failed.retry, true); assert.match(failed.text, /model offline/);
  assert.equal(ui.learnStatus({active_question: 'q'}).kind, 'waiting');
});

test('a missed turn is started once per view revision, never over a question or pause', () => {
  const owed = {needs_turn: true, running: false, paused: false, active_question: null, last_seq: 7};
  assert.equal(ui.learnShouldAutoTurn(owed, null), true);
  assert.equal(ui.learnShouldAutoTurn(owed, 7), false);
  assert.equal(ui.learnShouldAutoTurn({...owed, running: true}, null), false);
  assert.equal(ui.learnShouldAutoTurn({...owed, paused: true}, null), false);
  assert.equal(ui.learnShouldAutoTurn({...owed, active_question: 'q'}, null), false);
});

test('answers are validated before sending; optional signals are kept', () => {
  assert.equal(ui.learnBuildAnswer('single', {choices: []}).error, 'Choose an option first.');
  assert.equal(ui.learnBuildAnswer('single', {choices: ['a', 'b']}).error, 'Choose one option.');
  assert.deepEqual(ui.learnBuildAnswer('multiple', {choices: ['a', 'b'], confidence: '', thinking: ' '}).answer, {choices: ['a', 'b']});
  assert.equal(ui.learnBuildAnswer('free_recall', {text: '  '}).error, 'Write your answer first.');
  assert.deepEqual(ui.learnBuildAnswer('free_recall', {text: ' x ', confidence: '4', thinking: 'why'}).answer,
    {text: 'x', confidence: 4, thinking: 'why'});
});

test('grade text never claims mastery', () => {
  for (const grade of ['correct', 'incorrect', 'needs_judgment']) assert.doesNotMatch(ui.learnGradeText({grade}), /master/i);
});

test('free-recall judgments show the verdict and the missing relation, never a guess', () => {
  const judged = verdict => ({grade: 'needs_judgment', judgment: {verdict, missing: 'a full queue drops packets', feedback: 'You named buffers.'}});
  assert.equal(ui.learnGradeText(judged('partial')), 'Partly there.');
  assert.equal(ui.learnVerdict(judged('partial')), 'partial');
  assert.deepEqual(ui.learnJudgmentLines(judged('incorrect')), ['Missing: a full queue drops packets', 'You named buffers.']);
  assert.deepEqual(ui.learnJudgmentLines(judged('correct')), ['You named buffers.']);
  assert.match(ui.learnGradeText({grade: 'needs_judgment', judgment: {verdict: 'unclear', follow_up: 'Why?'}}), /follow-up/);
  assert.match(ui.learnGradeText({grade: 'needs_judgment', judgment: {verdict: 'unclear'}}), /Still not clear/);
  const failed = {grade: 'needs_judgment', judgment: {verdict: 'could_not_judge'}};
  assert.match(ui.learnGradeText(failed), /could not judge/);
  assert.deepEqual(ui.learnJudgmentLines(failed), []);
  for (const verdict of ['correct', 'partial', 'incorrect', 'unclear', 'could_not_judge']) {
    assert.doesNotMatch(ui.learnGradeText(judged(verdict)), /master/i);
  }
});
