'use strict';
// Exercise the real review handler with a small DOM boundary, without a window or CLI.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

function review() {
  let receive;
  const replies = [];
  const texts = { h2: {}, p: {}, pre: {} };
  const buttons = [{}, {}];
  const dialog = {
    setAttribute() {},
    querySelector: (s) => texts[s], querySelectorAll: () => buttons,
    showModal() { this.open = true; },
    close() { this.open = false; this.onclose?.(); },
    remove() { this.removed = true; },
  };
  const context = {
    window: { tune: {
      onHandoffReview(fn) { receive = fn; },
      aiHandoffReview: async (id, proceed) => { replies.push([id, proceed]); },
    } },
    document: { createElement: () => dialog, body: { append() {} } },
  };
  vm.runInNewContext(fs.readFileSync(require.resolve('../renderer/handoff-review.js'), 'utf8'), context);
  const payload = { id: 'review-a', title: 'A <img src=x onerror=alert(1)>', detail: 'vendor <script>bad()</script>', prompt: '```\n<&>\n```\nLAST MARKER' };
  receive(payload);
  return { dialog, texts, buttons, replies, payload };
}

test('the review preserves adversarial text as text and submits only the request id and decision once', () => {
  const r = review();
  assert.equal(r.texts.h2.textContent, r.payload.title);
  assert.equal(r.texts.p.textContent, r.payload.detail);
  assert.equal(r.texts.pre.textContent, r.payload.prompt);
  assert.doesNotMatch(r.dialog.innerHTML, /onerror|bad\(\)|LAST MARKER/);
  assert.deepEqual(r.replies, []);
  r.buttons[1].onclick();
  r.buttons[1].onclick();
  r.dialog.onclose();
  assert.deepEqual(r.replies, [['review-a', true]]);
  assert.equal(r.dialog.open, false);
  assert.equal(r.dialog.removed, true);
});

for (const cause of ['cancel button', 'Escape', 'Enter', 'close']) {
  test(`${cause} cancels once and cannot turn into approval`, () => {
    const r = review();
    let prevented = false;
    const event = { key: 'Enter', preventDefault() { prevented = true; } };
    if (cause === 'cancel button') r.buttons[0].onclick();
    else if (cause === 'Escape') r.dialog.oncancel(event);
    else if (cause === 'Enter') r.dialog.onkeydown(event);
    else r.dialog.onclose();
    r.buttons[1].onclick();
    assert.deepEqual(r.replies, [['review-a', false]]);
    if (cause === 'Escape' || cause === 'Enter') assert.equal(prevented, true);
    assert.equal(r.dialog.removed, true);
  });
}
