import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { test } from 'node:test';

// Execute the component's refresh function, with deferred IPC responses.
const source = readFileSync(new URL('../src/lib/components/HarnessPanel.svelte', import.meta.url), 'utf8');
const refresh = source.match(/  async function refresh[\s\S]*?\n  }\n/)[0];

test('polling stays serial and follows the latest workspace after a slow response', async () => {
  const calls = [];
  const pending = [];
  const state = {
    workspaceId: 'A', refreshing: false, requestGeneration: 0,
    loading: false, dashboard: null, errorMessage: '',
    getHarnessDashboard(id) {
      calls.push(id);
      return new Promise((resolve, reject) => pending.push({ resolve, reject }));
    },
  };
  runInNewContext(`${refresh}\nglobalThis.refresh = refresh;`, state);
  const first = state.refresh();
  void state.refresh();
  state.workspaceId = 'B';
  void state.refresh();
  assert.deepEqual(calls, ['A']);
  pending.shift().resolve({ workspaceId: 'A' });
  await first;
  assert.deepEqual(calls, ['A', 'B']);
  assert.equal(state.dashboard, null);
  pending.shift().resolve({ workspaceId: 'B' });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(state.dashboard.workspaceId, 'B');
  assert.equal(state.refreshing, false);

  const failed = state.refresh();
  pending.shift().reject(new Error('IPC failed'));
  await failed;
  assert.equal(state.refreshing, false);
  const retried = state.refresh();
  pending.shift().resolve({ workspaceId: 'B' });
  await retried;
  assert.equal(state.errorMessage, '');
});
