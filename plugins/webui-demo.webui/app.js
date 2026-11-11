const out = document.getElementById('out');

async function call() {
  out.textContent = '请求中…';
  try {
    const r = await fetch('./api/hello', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ who: 'WebUI' }),
    });
    out.textContent = JSON.stringify(await r.json(), null, 2);
  } catch (e) {
    out.textContent = '失败: ' + e.message;
  }
}

document.getElementById('go').addEventListener('click', call);
// 截图/自测用：?auto=1 时自己点一下
if (location.search.includes('auto=1')) call();
