// Settings > Models default-model picker: option labels and status text.
const test = require('node:test');
const assert = require('node:assert/strict');
const { modelOptionLabel, modelStatusText } = require('../web/model-picker.js');

const qwen = { id: 'qwen', name: 'Qwen3.6 35B-A3B', size_gb: 20.4, active_params_b: 3, total_params_b: 35, installed: true, running: true };
const lfm = { id: 'lfm', name: 'LFM3 Omni 2B', size_gb: 1.1, active_params_b: 2.2, total_params_b: 2.2, installed: false, running: false };

test('labels show size, active parameters and state', () => {
  assert.equal(modelOptionLabel(qwen), 'Qwen3.6 35B-A3B (35B, 3B active · 20.4 GB) — running');
  assert.equal(modelOptionLabel(lfm), 'LFM3 Omni 2B (2.2B · 1.1 GB) — not installed');
});

test('status says what runs now and what starts next', () => {
  const view = { models: [qwen, { ...lfm, installed: true }], running: 'qwen', default: 'qwen', restart_required: false };
  assert.equal(modelStatusText(view), 'Running Qwen3.6 35B-A3B, the saved default.');
  assert.equal(
    modelStatusText({ ...view, default: 'lfm', restart_required: true }),
    'Running Qwen3.6 35B-A3B. LFM3 Omni 2B is saved as the default and starts the next time SAIL starts.',
  );
  assert.match(modelStatusText({ ...view, env_override: true }), /SAIL_MODEL_PATH is set/);
  assert.equal(modelStatusText(null), 'Model list unavailable.');
});
