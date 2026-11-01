'use strict';
// Rust が作った全文を文字として表示する。ここで進んでも起動せず、ネイティブ確認へ進むだけ。
if (window.tune.onHandoffReview) window.tune.onHandoffReview(({ id, title, detail, prompt }) => {
  const dialog = document.createElement('dialog');
  dialog.className = 'handoff-review';
  dialog.setAttribute('aria-labelledby', 'handoff-title');
  dialog.innerHTML = '<h2 id="handoff-title"></h2><p class="handoff-detail"></p><pre tabindex="0" aria-label="渡すプロンプトの全文"></pre><div class="handoff-buttons"><button class="btn" autofocus>やめる</button><button class="btn primary">確認へ進む</button></div>';
  dialog.querySelector('h2').textContent = title;
  dialog.querySelector('p').textContent = detail;
  dialog.querySelector('pre').textContent = prompt;
  let answered = false;
  const answer = (proceed) => {
    if (answered) return;
    answered = true;
    dialog.close();
    dialog.remove();
    window.tune.aiHandoffReview(id, proceed).catch(() => {});
  };
  const buttons = dialog.querySelectorAll('button');
  buttons[0].onclick = () => answer(false);
  buttons[1].onclick = () => answer(true);
  dialog.oncancel = (event) => { event.preventDefault(); answer(false); };
  dialog.onclose = () => answer(false);
  // Enter の暗黙の承認を避ける。確認へ進むボタン上でも、クリックか Space を使う。
  dialog.onkeydown = (event) => { if (event.key === 'Enter') { event.preventDefault(); answer(false); } };
  document.body.append(dialog);
  dialog.showModal();
});
