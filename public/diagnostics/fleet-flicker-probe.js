// Load from the Web Inspector Console as described in README.md.
// This observes selected DOM state, not pixels or backend events.
(() => {
  window.__claiFlicker?.stop();
  const started = performance.now();
  const rows = [];
  const ids = new WeakMap();
  let nextId = 1;
  let previous = '';
  let mutations = 0;
  const nodeId = (node) => {
    if (!node) return null;
    if (!ids.has(node)) ids.set(node, nextId++);
    return ids.get(node);
  };
  const text = (node) => node?.textContent?.trim().replace(/\s+/g, ' ') ?? null;
  const findText = (selector, pattern) =>
    [...document.querySelectorAll(selector)].find((node) => pattern.test(text(node) ?? ''));
  const snapshot = () => {
    const counter = findText('span', /^\d+ need attention$/);
    const attention = findText('[role="presentation"]', /^Needs attention$/);
    const recent = findText('[role="presentation"]', /^Recent$/);
    const title = document.querySelector('h1');
    const chat = document.querySelector('[class*="chatFirstContent"]');
    const taskBar = findText('button', /^\d+ tasks\b/);
    const row = {
      ms: Math.round(performance.now() - started),
      path: location.pathname,
      visibility: document.visibilityState,
      focus: document.hasFocus(),
      counter: text(counter),
      counterNode: nodeId(counter),
      attention: !!attention,
      attentionNode: nodeId(attention),
      recent: !!recent,
      title: text(title),
      titleNode: nodeId(title),
      chatNode: nodeId(chat),
      chatChildren: chat?.children.length ?? null,
      taskBar: text(taskBar),
      taskBarNode: nodeId(taskBar),
    };
    const key = JSON.stringify({ ...row, ms: 0 });
    if (key !== previous) {
      rows.push({ ...row, mutations });
      console.log('CLAI flicker', rows.at(-1));
      previous = key;
      mutations = 0;
    }
  };
  const observer = new MutationObserver((records) => {
    mutations += records.length;
    snapshot();
  });
  observer.observe(document.body, { subtree: true, childList: true, characterData: true, attributes: true });
  const timer = setInterval(snapshot, 100);
  const onFocus = () => snapshot();
  window.addEventListener('focus', onFocus);
  window.addEventListener('blur', onFocus);
  document.addEventListener('visibilitychange', onFocus);
  window.__claiFlicker = {
    rows,
    stop() {
      observer.disconnect();
      clearInterval(timer);
      window.removeEventListener('focus', onFocus);
      window.removeEventListener('blur', onFocus);
      document.removeEventListener('visibilitychange', onFocus);
      snapshot();
      console.log('CLAI flicker capture stopped:', rows.length, 'state changes');
    },
  };
  snapshot();
})();
