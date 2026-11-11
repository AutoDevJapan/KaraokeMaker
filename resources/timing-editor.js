(() => {
  'use strict';

  const copy = lines => lines.map(line => ({ ...line }));
  const formatTime = cs => `${String(Math.floor(cs / 6000)).padStart(2, '0')}:${String(Math.floor(cs / 100) % 60).padStart(2, '0')}.${String(cs % 100).padStart(2, '0')}`;
  function parseTime(text) {
    const match = /^(?:(\d+):([0-5]\d)|(\d+))(?:\.(\d{1,2}))?$/.exec(text.trim());
    if (!match) throw new Error('時刻は 00:12.34 または秒数で入力してください。');
    const value = (match[1] === undefined ? Number(match[3]) : Number(match[1]) * 60 + Number(match[2])) * 100 + Number((match[4] || '').padEnd(2, '0'));
    if (!Number.isSafeInteger(value)) throw new Error('時刻が大きすぎます。');
    return value;
  }
  function lrc(lines) {
    const cues = [];
    for (const line of lines) {
      if (cues.length && cues[cues.length - 1][0] === line.start) cues.pop();
      for (const text of line.text.split('\n')) cues.push([line.start, text]);
      cues.push([line.end, '']);
    }
    return cues.map(([cs, text]) => `[${formatTime(cs)}]${text}\n`).join('');
  }

  class TimingEditor {
    #lines;
    #undo = [];
    #initial;
    constructor(draft) {
      if (typeof draft.duration !== 'number' || !Number.isFinite(draft.duration) || draft.duration <= 0 || !Array.isArray(draft.lines)) throw new Error('曲の長さか歌詞を読み込めません。');
      this.duration = draft.duration;
      this.end = Math.round(draft.duration * 100);
      this.#lines = draft.lines.map(line => {
        if (typeof line.start !== 'number' || typeof line.end !== 'number' || !Number.isFinite(line.start) || !Number.isFinite(line.end) || line.start < 0 || line.start >= line.end || line.end > this.duration) throw new Error('曲の範囲外の時刻があります。');
        return { start: Math.round(line.start * 100), end: Math.round(line.end * 100), text: line.text };
      });
      this.#validate(this.#lines);
      this.#initial = JSON.stringify(this.#lines);
    }
    #validate(lines) {
      if (!lines.length) throw new Error('歌詞がありません。下のLRC欄から歌詞を保存してください。');
      let previousEnd = 0;
      for (const line of lines) {
        if (!Number.isSafeInteger(line.start) || !Number.isSafeInteger(line.end) || line.start < 0 || line.end > this.end) throw new Error('曲の範囲内の時刻にしてください。');
        if (line.start >= line.end) throw new Error('開始より終了を0.01秒以上後にしてください。');
        if (line.start < previousEnd) throw new Error('歌詞の時刻が重複しています。');
        if (typeof line.text !== 'string' || !line.text.trim() || line.text.split('\n').some(text => text.trimStart().startsWith('['))) throw new Error('歌詞の空行や行頭の [ はLRC欄で確認してください。');
        previousEnd = line.end;
      }
      if (new TextEncoder().encode(lrc(lines)).length > 65536) throw new Error('時刻付きの歌詞は64KiBまでです。');
    }
    get lines() { return copy(this.#lines); }
    get canUndo() { return this.#undo.length > 0; }
    get changed() { return JSON.stringify(this.#lines) !== this.#initial; }
    replace(index, start, end, text = this.#lines[index]?.text) {
      if (!Number.isInteger(index) || index < 0 || index >= this.#lines.length) throw new Error('歌詞の行を選んでください。');
      const next = copy(this.#lines);
      next[index] = { start, end, text };
      this.#validate(next);
      if (JSON.stringify(next) !== JSON.stringify(this.#lines)) {
        this.#undo.push(this.#lines);
        if (this.#undo.length > 100) this.#undo.shift();
        this.#lines = next;
      }
    }
    undo() {
      if (!this.#undo.length) return false;
      this.#lines = this.#undo.pop();
      return true;
    }
    lrc() { return lrc(this.#lines); }
  }

  async function audition(media, line) {
    const end = line.end / 100;
    const stop = () => media.removeEventListener('timeupdate', tick);
    const tick = () => {
      if (media.currentTime >= end) {
        media.pause();
        media.currentTime = end;
        stop();
      }
    };
    media.currentTime = line.start / 100;
    media.addEventListener('timeupdate', tick);
    try { await media.play(); } catch (error) { stop(); throw error; }
    return stop;
  }

  async function mount(root) {
    const find = selector => root.querySelector(selector);
    const video = find('video');
    const message = find('[role="alert"]');
    const start = find('[name="line-start"]');
    const end = find('[name="line-end"]');
    const rows = find('tbody');
    const seek = find('[name="playhead"]');
    const clock = find('.timing-clock');
    const play = find('[data-play]');
    const undo = find('[data-undo]');
    const save = find('[data-save]');
    const form = find('.timing-save');
    const displayError = error => { message.textContent = error ? error.message || String(error) : ''; message.hidden = !error; };
    let editor;
    let selected = 0;
    let savedUndo = false;
    let stopAudition = () => {};
    let submitting = false;
    let invalid = false;
    const time = () => {
      const cs = Math.round(video.currentTime * 100);
      clock.textContent = formatTime(cs);
      seek.value = String(video.currentTime);
      play.textContent = video.paused ? '▶ 再生' : '一時停止';
      const lines = editor?.lines;
      if (lines) for (const [index, row] of [...rows.children].entries()) {
        const line = lines[index];
        row.classList.toggle('playing', cs >= line.start && cs < line.end);
        row.style.setProperty('--playhead', `${Math.min(100, Math.max(0, video.currentTime / editor.duration * 100))}%`);
      }
    };
    function refresh() {
      const lines = editor.lines;
      rows.replaceChildren();
      lines.forEach((line, index) => {
        const row = document.createElement('tr');
        row.classList.toggle('selected', index === selected);
        const number = document.createElement('td');
        const select = document.createElement('button');
        select.type = 'button'; select.className = 'secondary'; select.textContent = String(index + 1);
        select.setAttribute('aria-label', `${index + 1}行目を選ぶ`);
        select.setAttribute('aria-pressed', String(index === selected));
        select.addEventListener('click', () => { selected = index; invalid = false; displayError(null); refresh(); start.focus(); });
        number.append(select);
        const times = document.createElement('td'); times.textContent = `${formatTime(line.start)} – ${formatTime(line.end)}`;
        const text = document.createElement('td'); text.textContent = line.text;
        const barCell = document.createElement('td'); barCell.className = 'timing-bar-cell';
        const bar = document.createElement('span'); bar.className = 'timing-bar';
        bar.style.left = `${line.start / editor.end * 100}%`;
        bar.style.width = `${(line.end - line.start) / editor.end * 100}%`;
        barCell.append(bar); row.append(number, times, text, barCell); rows.append(row);
      });
      const line = lines[selected];
      start.value = formatTime(line.start); end.value = formatTime(line.end);
      start.removeAttribute('aria-invalid'); end.removeAttribute('aria-invalid');
      find('.timing-selection').textContent = `${selected + 1}行目：${line.text}`;
      undo.disabled = submitting || (!editor.canUndo && !savedUndo);
      undo.title = editor.canUndo ? '保存前の変更を1段戻す' : '前回保存した編集を取り消して別の結果を作る';
      save.disabled = submitting || invalid;
      form.elements.lyrics.value = editor.lrc();
      time();
    }
    function edit(change) {
      if (!editor || submitting) return false;
      try {
        let a = parseTime(start.value), b = parseTime(end.value);
        if (change) [a, b] = change(a, b);
        editor.replace(selected, a, b);
        invalid = false; displayError(null); stopAudition(); refresh(); return true;
      } catch (error) {
        invalid = true; displayError(error); save.disabled = true;
        start.setAttribute('aria-invalid', 'true'); end.setAttribute('aria-invalid', 'true'); return false;
      }
    }
    for (const input of [start, end]) input.addEventListener('change', () => edit());
    for (const button of root.querySelectorAll('[data-step]')) button.addEventListener('click', () => {
      const delta = Number(button.dataset.step);
      edit((a, b) => button.dataset.edge === 'start' ? [a + delta, b] : [a, b + delta]);
    });
    find('[data-use-playhead]').addEventListener('click', () => edit((a, b) => [Math.round(video.currentTime * 100), b]));
    seek.addEventListener('input', () => { stopAudition(); video.currentTime = Number(seek.value); time(); });
    play.addEventListener('click', async () => {
      stopAudition();
      if (!video.paused) video.pause();
      else try { await video.play(); } catch (error) { displayError(error); }
    });
    find('[data-audition]').addEventListener('click', async () => {
      if (!edit()) return;
      try { stopAudition(); stopAudition = await audition(video, editor.lines[selected]); } catch (error) { displayError(error); }
    });
    for (const event of ['timeupdate', 'play', 'pause', 'seeked']) video.addEventListener(event, time);
    undo.addEventListener('click', () => {
      if (!editor || submitting) return;
      stopAudition(); video.pause(); invalid = false; displayError(null);
      if (editor.undo()) refresh();
      else if (savedUndo) { submitting = true; find('.timing-undo').requestSubmit(); }
    });
    form.addEventListener('submit', event => {
      if (!edit() || submitting) { event.preventDefault(); return; }
      submitting = true; save.disabled = true; undo.disabled = true; video.pause();
      for (const input of root.querySelectorAll('fieldset')) input.disabled = true;
      // CSRF, preset and lyrics live outside the disabled editor fieldset.
    });
    window.addEventListener('beforeunload', event => {
      if (editor?.changed && !submitting) { event.preventDefault(); event.returnValue = ''; }
    });
    try {
      const response = await fetch(root.dataset.timingUrl, { credentials: 'same-origin', headers: { Accept: 'application/json' } });
      if (!response.ok) throw new Error('保存済みの歌詞時刻を読み込めません。下のLRC欄で確認してください。保存済み履歴は変更していません。');
      const draft = await response.json();
      editor = new TimingEditor(draft); savedUndo = draft.can_undo === true;
      seek.max = String(editor.duration);
      for (const fieldset of root.querySelectorAll('fieldset')) fieldset.disabled = false;
      refresh(); root.dataset.ready = 'true';
    } catch (error) { displayError(error); root.dataset.ready = 'error'; }
  }

  if (typeof module !== 'undefined') module.exports = { TimingEditor, parseTime, formatTime, audition, mount };
  if (typeof document !== 'undefined') for (const root of document.querySelectorAll('.timing-editor')) mount(root);
})();
