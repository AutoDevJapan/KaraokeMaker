const test = require('node:test');
const assert = require('node:assert/strict');
const { TimingEditor, parseTime, formatTime, audition, mount } = require('../resources/timing-editor.js');

const initial = () => ({ duration: 5, lines: [
  { start: 0.2, end: 1.2, text: '灯りの下で' },
  { start: 2, end: 4, text: '明日へ歩く' },
], can_undo: true });

test('editing, rejected overlaps, local undo and rests preserve the original input', () => {
  const input = initial();
  const original = JSON.stringify(input);
  const editor = new TimingEditor(input);
  editor.replace(0, 30, 130);
  assert.equal(editor.lrc(), '[00:00.30]灯りの下で\n[00:01.30]\n[00:02.00]明日へ歩く\n[00:04.00]\n');
  assert.throws(() => editor.replace(0, 30, 201), /重複/);
  assert.throws(() => editor.replace(1, 200, 501), /曲の範囲/);
  assert.equal(editor.lines[0].end, 130);
  assert.equal(editor.undo(), true);
  assert.equal(editor.lines[0].start, 20);
  assert.equal(editor.undo(), false);
  assert.equal(JSON.stringify(input), original);
});

test('centisecond input survives minute and rounded song-end boundaries', () => {
  assert.equal(parseTime('01:08.04'), 6804);
  assert.equal(parseTime('68.04'), 6804);
  assert.equal(formatTime(6804), '01:08.04');
  for (const bad of ['', 'Infinity', '-0.1', '1e2', '00:60.00', '0.001']) {
    assert.throws(() => parseTime(bad), /時刻/);
  }
  const editor = new TimingEditor({ duration: 68.036, lines: [{ start: 67.99, end: 68.036, text: '曲末' }] });
  assert.equal(editor.lrc(), '[01:07.99]曲末\n[01:08.04]\n');
  assert.throws(() => editor.replace(0, 6804, 6804), /開始/);
  assert.throws(() => editor.replace(0, 6799, 6805), /曲の範囲/);
});

test('empty, invalid and ambiguous drafts cannot silently replace lyrics', () => {
  for (const input of [
    { duration: 5, lines: [] },
    { duration: NaN, lines: initial().lines },
    { duration: 5, lines: [{ start: 0, end: 1, text: '[00:02]偽の時刻' }] },
    { duration: 5, lines: [{ start: 0, end: 0.001, text: '短すぎる' }] },
  ]) assert.throws(() => new TimingEditor(input));
  const editor = new TimingEditor(initial());
  const lines = editor.lines;
  lines[0].text = '外部から改変';
  assert.equal(editor.lines[0].text, '灯りの下で');
  editor.replace(0, 20, 120, 'a <script>\nb & c');
  assert.match(editor.lrc(), /a <script>\n\[00:00.20\]b & c/);
});

test('audition uses the selected interval and pauses at its end', async () => {
  const callbacks = new Map();
  const media = { currentTime: 0, paused: true,
    addEventListener(name, fn) { callbacks.set(name, fn); },
    removeEventListener(name) { callbacks.delete(name); },
    async play() { this.paused = false; },
    pause() { this.paused = true; },
  };
  const stop = await audition(media, { start: 125, end: 200 });
  assert.equal(media.currentTime, 1.25);
  assert.equal(media.paused, false);
  media.currentTime = 2.1;
  callbacks.get('timeupdate')();
  assert.equal(media.paused, true);
  assert.equal(media.currentTime, 2);
  stop();
  assert.equal(callbacks.size, 0);
});

// Minimal DOM ports for the actual controller's event handlers. Rendering and
// browser media behavior are separate acceptance checks, not inferred here.
class Element {
  children = [];
  listeners = new Map();
  attributes = new Map();
  dataset = {};
  value = '';
  disabled = false;
  textContent = '';
  style = { setProperty() {} };
  classList = { toggle() {} };
  addEventListener(name, fn) { this.listeners.set(name, fn); }
  removeEventListener(name) { this.listeners.delete(name); }
  setAttribute(name, value) { this.attributes.set(name, value); }
  removeAttribute(name) { this.attributes.delete(name); }
  replaceChildren(...nodes) { this.children = nodes; }
  append(...nodes) { this.children.push(...nodes); }
  focus() {}
  async fire(name) {
    const event = { defaultPrevented: false, preventDefault() { this.defaultPrevented = true; } };
    await this.listeners.get(name)?.(event);
    return event;
  }
}

function domFixture(t, response = initial()) {
  const selectors = ['video', '[role="alert"]', '[name="line-start"]', '[name="line-end"]',
    'tbody', '[name="playhead"]', '.timing-clock', '[data-play]', '[data-undo]', '[data-save]',
    '.timing-save', '.timing-undo', '[data-use-playhead]', '[data-audition]', '.timing-selection'];
  const nodes = new Map(selectors.map(name => [name, new Element()]));
  const video = nodes.get('video');
  video.currentTime = 0; video.paused = true;
  video.play = async () => { video.paused = false; };
  video.pause = () => { video.paused = true; };
  nodes.get('.timing-save').elements = { lyrics: { value: '' } };
  nodes.get('.timing-undo').requestSubmit = () => { nodes.get('.timing-undo').submitted = true; };
  const fieldset = new Element(); fieldset.disabled = true;
  const root = { dataset: { timingUrl: '/jobs/synthetic/timing' }, querySelector: s => nodes.get(s),
    querySelectorAll: s => s === 'fieldset' ? [fieldset] : [] };
  const old = { document: global.document, window: global.window, fetch: global.fetch };
  global.document = { createElement: () => new Element() };
  global.window = { addEventListener() {} };
  global.fetch = async (url, options) => {
    assert.equal(url, root.dataset.timingUrl);
    assert.equal(options.credentials, 'same-origin');
    return { ok: response !== null, json: async () => response };
  };
  t.after(() => Object.assign(global, old));
  return { nodes, root, fieldset };
}

test('DOM editing, selection, invalid-save prevention and exact POST lyrics use the real controller', async t => {
  const { nodes, root, fieldset } = domFixture(t);
  await mount(root);
  assert.equal(root.dataset.ready, 'true');
  assert.equal(fieldset.disabled, false);
  assert.equal(nodes.get('tbody').children.length, 2);
  const start = nodes.get('[name="line-start"]');
  const end = nodes.get('[name="line-end"]');
  start.value = '0.30'; await start.fire('change');
  assert.equal(start.value, '00:00.30');
  assert.match(nodes.get('.timing-save').elements.lyrics.value, /^\[00:00.30\]/);
  await nodes.get('[data-undo]').fire('click');
  assert.equal(start.value, '00:00.20');
  assert.equal(nodes.get('[role="alert"]').hidden, true);
  end.value = '2.50'; await end.fire('change');
  assert.match(nodes.get('[role="alert"]').textContent, /重複/);
  assert.equal((await nodes.get('.timing-save').fire('submit')).defaultPrevented, true);
  assert.equal(nodes.get('[data-save]').disabled, true);
  end.value = '1.30'; await end.fire('change');
  const selectSecond = nodes.get('tbody').children[1].children[0].children[0];
  await selectSecond.fire('click');
  assert.equal(start.value, '00:02.00');
  nodes.get('video').currentTime = 2.25;
  await nodes.get('[data-use-playhead]').fire('click');
  assert.equal(start.value, '00:02.25');
  assert.equal((await nodes.get('.timing-save').fire('submit')).defaultPrevented, false);
  assert.match(nodes.get('.timing-save').elements.lyrics.value, /\[00:02.25\]明日へ歩く\n\[00:04.00\]/);
  assert.equal((await nodes.get('.timing-save').fire('submit')).defaultPrevented, true);
  assert.equal(nodes.get('[data-save]').disabled, true);
});

test('server undo is requested only after local edits are undone', async t => {
  const { nodes, root } = domFixture(t);
  await mount(root);
  nodes.get('[name="line-start"]').value = '0.30';
  await nodes.get('[name="line-start"]').fire('change');
  await nodes.get('[data-undo]').fire('click');
  assert.equal(nodes.get('.timing-undo').submitted, undefined);
  await nodes.get('[data-undo]').fire('click');
  assert.equal(nodes.get('.timing-undo').submitted, true);
});

test('failed loading leaves editing disabled without silently replacing history', async t => {
  const broken = domFixture(t, null);
  await mount(broken.root);
  assert.equal(broken.root.dataset.ready, 'error');
  assert.equal(broken.fieldset.disabled, true);
  assert.match(broken.nodes.get('[role="alert"]').textContent, /保存済み履歴は変更していません/);
});
