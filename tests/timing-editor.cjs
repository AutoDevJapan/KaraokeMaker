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
  const operation = audition(media, { start: 125, end: 200 });
  await operation.ready;
  assert.equal(media.currentTime, 1.25);
  assert.equal(media.paused, false);
  media.currentTime = 2.1;
  callbacks.get('timeupdate')();
  assert.equal(media.paused, true);
  assert.equal(media.currentTime, 2);
  operation.stop();
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
  const windowEvents = new Map();
  global.window = { addEventListener(name, listener) { windowEvents.set(name, listener); } };
  global.fetch = async (url, options) => {
    assert.equal(url, root.dataset.timingUrl);
    assert.equal(options.credentials, 'same-origin');
    return { ok: response !== null, json: async () => response };
  };
  t.after(() => Object.assign(global, old));
  return { nodes, root, fieldset, windowEvents };
}

function deferred() {
  let resolve, reject;
  const promise = new Promise((a, b) => { resolve = a; reject = b; });
  return { promise, resolve, reject };
}

test('pending audition A cannot stop audition B after A resolves', async t => {
  const { nodes, root } = domFixture(t);
  await mount(root);
  const video = nodes.get('video');
  const listeners = new Set();
  video.addEventListener = (event, fn) => { if (event === 'timeupdate') listeners.add(fn); };
  video.removeEventListener = (event, fn) => { if (event === 'timeupdate') listeners.delete(fn); };
  const a = deferred(), b = deferred();
  let calls = 0;
  video.play = () => { video.paused = false; return ++calls === 1 ? a.promise : b.promise; };
  const first = nodes.get('[data-audition]').fire('click');
  assert.equal(listeners.size, 1);
  const selectSecond = nodes.get('tbody').children[1].children[0].children[0];
  await selectSecond.fire('click');
  const second = nodes.get('[data-audition]').fire('click');
  assert.equal(listeners.size, 1, 'only B may retain an interval listener');
  a.resolve(); await first;
  video.currentTime = 2.5;
  for (const listener of [...listeners]) listener();
  assert.equal(video.paused, false, 'A end=1.2 must not stop B end=4');
  b.resolve(); await second;
  video.currentTime = 4.2;
  for (const listener of [...listeners]) listener();
  assert.equal(video.paused, true);
  assert.equal(video.currentTime, 4);
  assert.equal(listeners.size, 0);
});

test('seek cancels a pending audition immediately; its later rejection is stale', async t => {
  const { nodes, root } = domFixture(t);
  await mount(root);
  const video = nodes.get('video');
  const pending = deferred();
  video.play = () => { video.paused = false; return pending.promise; };
  const first = nodes.get('[data-audition]').fire('click');
  nodes.get('[name="playhead"]').value = '3';
  await nodes.get('[name="playhead"]').fire('input');
  assert.equal(video.paused, true);
  pending.reject(new Error('stale play interrupted')); await first;
  assert.equal(nodes.get('[role="alert"]').hidden, true);
  assert.equal(video.currentTime, 3);
});

test('an already queued pause event from A does not cancel newer playing audition B', async () => {
  const listeners = new Map();
  const queued = [];
  const media = {
    currentTime: 0, paused: true,
    addEventListener(name, fn) { if (!listeners.has(name)) listeners.set(name, new Set()); listeners.get(name).add(fn); },
    removeEventListener(name, fn) { listeners.get(name)?.delete(fn); },
    async play() { this.paused = false; },
    pause() { if (!this.paused) { this.paused = true; queued.push('pause'); } },
  };
  const first = audition(media, { start: 20, end: 120 }); await first.ready;
  first.stop();
  const second = audition(media, { start: 200, end: 400 }); await second.ready;
  for (const name of queued.splice(0)) for (const listener of [...listeners.get(name)]) listener();
  assert.equal(media.paused, false, 'pause is queued by the media standard; B is already playing');
  assert.equal(listeners.get('timeupdate').size, 1);
  second.stop();
  assert.equal(listeners.get('timeupdate').size, 0);
});

test('native video seeking cancels audition without snapping to the old interval end', async () => {
  const handlers = new Map();
  const media = { currentTime: 0, paused: true, seeking: false,
    addEventListener(name, fn) { if (!handlers.has(name)) handlers.set(name, new Set()); handlers.get(name).add(fn); },
    removeEventListener(name, fn) { handlers.get(name)?.delete(fn); },
    async play() { this.paused = false; },
    pause() { this.paused = true; },
  };
  const fire = event => { for (const handler of [...(handlers.get(event) || [])]) handler(); };
  const operation = audition(media, { start: 125, end: 200 });
  // The programmatic initial seek also fires seeking; it must keep the audition.
  media.seeking = true; fire('seeking'); media.seeking = false; fire('seeked'); await operation.ready;
  assert.equal(media.paused, false);
  media.currentTime = 3.5; media.seeking = true;
  fire('timeupdate'); // a previously queued timeupdate can run before the new seeking task
  fire('seeking'); media.seeking = false; fire('seeked');
  assert.equal(media.currentTime, 3.5, 'native seek beyond the line keeps the requested position');
  assert.equal(media.paused, true);
  assert.equal(handlers.get('timeupdate').size, 0);
  // A no-op initial assignment may emit no seeked event. A later native seek
  // inside the line still cancels without changing the requested position.
  media.currentTime = 1.25;
  const next = audition(media, { start: 125, end: 200 }); await next.ready;
  media.currentTime = 1.6; media.seeking = true; fire('seeking'); media.seeking = false; fire('seeked'); fire('timeupdate');
  assert.equal(media.currentTime, 1.6);
  assert.equal(media.paused, true);
  assert.equal(handlers.get('timeupdate').size, 0);
});

test('old seek events do not cancel a new audition; seeking back to its start after playback does', async () => {
  const handlers = new Map();
  const media = { currentTime: 0, paused: true, seeking: true,
    addEventListener(name, fn) { if (!handlers.has(name)) handlers.set(name, new Set()); handlers.get(name).add(fn); },
    removeEventListener(name, fn) { handlers.get(name)?.delete(fn); },
    async play() { this.paused = false; }, pause() { this.paused = true; },
  };
  const fire = event => { for (const fn of [...(handlers.get(event) || [])]) fn(); };
  const first = audition(media, { start: 20, end: 120 }); await first.ready;
  first.stop();
  const second = audition(media, { start: 200, end: 400 }); await second.ready;
  fire('seeked'); // queued by A, observed by B's listeners
  fire('seeking'); fire('seeked'); // B's own programmatic initial seek
  media.seeking = false;
  assert.equal(media.paused, false);
  media.currentTime = 2.4; fire('timeupdate');
  assert.equal(media.paused, false);
  fire('seeking'); // old queued seeking task after the actual seek completed
  assert.equal(media.paused, false, 'completed native seek state makes an old event harmless');
  media.currentTime = 2; media.seeking = true; fire('seeking'); media.seeking = false; fire('seeked');
  assert.equal(media.currentTime, 2);
  assert.equal(media.paused, true, 'seeking to the start after playback advanced is a user seek');
  assert.equal(handlers.get('timeupdate').size, 0);
});

test('line selection, submit, undo, normal playback and edit cancel pending auditions', async t => {
  for (const action of ['select', 'save', 'undo', 'play', 'edit', 'invalid edit']) {
    await t.test(action, async t => {
      const { nodes, root } = domFixture(t);
      await mount(root);
      const video = nodes.get('video');
      const listeners = new Set();
      video.addEventListener = (event, fn) => { if (event === 'timeupdate') listeners.add(fn); };
      video.removeEventListener = (event, fn) => { if (event === 'timeupdate') listeners.delete(fn); };
      const pending = deferred();
      let playCalls = 0;
      video.play = () => { video.paused = false; return ++playCalls === 1 ? pending.promise : Promise.resolve(); };
      const attempt = nodes.get('[data-audition]').fire('click');
      assert.equal(listeners.size, 1);
      if (action === 'select') await nodes.get('tbody').children[1].children[0].children[0].fire('click');
      if (action === 'save') await nodes.get('.timing-save').fire('submit');
      if (action === 'undo') await nodes.get('[data-undo]').fire('click');
      if (action === 'play') await nodes.get('[data-play]').fire('click');
      if (action === 'play') assert.equal(video.paused, true, 'pause must not restart an active audition');
      if (action.endsWith('edit')) {
        nodes.get('[name="line-start"]').value = action === 'edit' ? '0.30' : '4.99';
        await nodes.get('[name="line-start"]').fire('change');
      }
      assert.equal(listeners.size, 0, `${action} must cancel before play settles`);
      pending.reject(new Error('cancelled play')); await attempt;
      if (action !== 'invalid edit') assert.equal(nodes.get('[role="alert"]').hidden, true);
    });
  }
});

test('table changes prompt before unload; only the table save suppresses that prompt', async t => {
  const { nodes, root, windowEvents } = domFixture(t);
  await mount(root);
  const leave = () => {
    const event = { blocked: false, preventDefault() { this.blocked = true; } };
    windowEvents.get('beforeunload')(event);
    return event.blocked;
  };
  assert.equal(leave(), false);
  nodes.get('[name="line-start"]').value = '0.30';
  await nodes.get('[name="line-start"]').fire('change');
  assert.equal(leave(), true, 'separate LRC-form navigation must not mark the table saved');
  await nodes.get('[data-undo]').fire('click');
  assert.equal(leave(), false);
  nodes.get('[name="line-start"]').value = '0.40';
  await nodes.get('[name="line-start"]').fire('change');
  await nodes.get('.timing-save').fire('submit');
  assert.equal(leave(), false);
});

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

test('undo of a rejected input restores the current row without consuming saved history', async t => {
  const { nodes, root } = domFixture(t);
  await mount(root);
  const start = nodes.get('[name="line-start"]');
  start.value = '4.99'; await start.fire('change');
  assert.equal(nodes.get('[role="alert"]').hidden, false);
  await nodes.get('[data-undo]').fire('click');
  assert.equal(nodes.get('.timing-undo').submitted, undefined);
  assert.equal(start.value, '00:00.20');
  assert.equal(nodes.get('[role="alert"]').hidden, true);
  await nodes.get('[data-undo]').fire('click');
  assert.equal(nodes.get('.timing-undo').submitted, true, 'saved undo remains available after input restoration');
});

test('a rejected input can also be restored when no saved or local undo exists', async t => {
  const { nodes, root } = domFixture(t, { ...initial(), can_undo: false });
  await mount(root);
  nodes.get('[name="line-start"]').value = '4.99'; await nodes.get('[name="line-start"]').fire('change');
  assert.equal(nodes.get('[data-undo]').disabled, false);
  await nodes.get('[data-undo]').fire('click');
  assert.equal(nodes.get('[name="line-start"]').value, '00:00.20');
  assert.equal(nodes.get('.timing-undo').submitted, undefined);
});

test('failed loading leaves editing disabled without silently replacing history', async t => {
  const broken = domFixture(t, null);
  await mount(broken.root);
  assert.equal(broken.root.dataset.ready, 'error');
  assert.equal(broken.fieldset.disabled, true);
  assert.match(broken.nodes.get('[role="alert"]').textContent, /保存済み履歴は変更していません/);
});
