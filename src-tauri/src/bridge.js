/* peaDrive — cầu nối giữa index.html và backend Rust.
 *
 * Được Tauri chèn vào mọi trang của cửa sổ chính TRƯỚC khi script của trang chạy, nên index.html giữ nguyên:
 *   - fetch('/api/…')  →  invoke('api', { method, url, body })  (không có HTTP server nào)
 *   - window.open(url) →  mở bằng trình duyệt mặc định của hệ điều hành
 *   - Ctrl/⌘ + ,       →  hộp thoại Cài đặt (Google OAuth + hiệu năng); tự mở khi chưa có Client ID
 */
(() => {
  'use strict';
  if (window.__peadriveBridge) return;
  window.__peadriveBridge = true;

  const nativeFetch = window.fetch.bind(window);
  const rpc = (method, url, body) => window.__TAURI__.core.invoke('api', { method, url, body: body === undefined ? null : body });
  const OAUTH_MARK = 'E_OAUTH_CONFIG:';
  const OUTDATED_MARK = 'E_OUTDATED:';

  const notify = (msg, isErr) => {
    if (typeof window.toast === 'function') window.toast(msg, !!isErr);
    else (isErr ? console.error : console.log)(msg);
  };

  /* ---------- fetch('/api/…') ---------- */
  window.fetch = async function (input, init) {
    init = init || {};
    const raw = typeof input === 'string' ? input : (input && input.url) || '';
    const path = raw.replace(/^[a-z][a-z0-9+.-]*:\/\/[^/]+/i, '');
    if (!path.startsWith('/api/')) return nativeFetch(input, init);

    let body;
    if (typeof init.body === 'string') {
      try { body = JSON.parse(init.body); } catch (_) { body = undefined; }
    }
    try {
      const data = await rpc(init.method || 'GET', path, body);
      if (path.startsWith('/api/status') && data && data.login_error) notify(data.login_error, true);
      if (data && ((path.startsWith('/api/update/check') && data.force) || (path.startsWith('/api/status') && data.blocked))) {
        showBlock({ url: data.url || data.update_url });
      }
      return new Response(JSON.stringify(data === undefined || data === null ? {} : data), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      });
    } catch (e) {
      let msg = typeof e === 'string' ? e : (e && e.message) || String(e);
      if (msg.startsWith(OUTDATED_MARK)) {
        // Phiên bản quá cũ: backend đã chặn mọi route. Hiện lớp chặn và để lời gọi treo (không toast, không lỗi dồn dập).
        showBlock();
        return new Promise(() => {});
      }
      if (msg.startsWith(OAUTH_MARK)) {
        msg = msg.slice(OAUTH_MARK.length);
        setTimeout(() => openSettings(), 60);
      }
      return new Response(msg, { status: 400, headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
    }
  };

  /* ---------- window.open → trình duyệt mặc định (https) ---------- */
  window.open = function (url) {
    if (typeof url === 'string' && /^https:\/\//i.test(url)) {
      rpc('POST', '/api/open?url=' + encodeURIComponent(url)).catch((e) => notify(String(e), true));
    }
    return null;
  };

  /* ---------- Ngôn ngữ: theo document.documentElement.lang (do setLang() của trang đặt) ---------- */
  const TXT = {
    en: {
      'Phiên bản ứng dụng của bạn đã cũ. Vui lòng cập nhật lên phiên bản mới nhất để tiếp tục sử dụng.': 'Your app version is out of date. Please update to the latest version to continue using it.',
      'Cập nhật ngay': 'Update now',
      'Cài đặt peaDrive': 'peaDrive Settings',
      'Hiệu năng tải và tuỳ chọn cá nhân. Phím tắt: Ctrl/⌘ + ,': 'Download performance and personal options. Shortcut: Ctrl/⌘ + ,',
      'Google OAuth và hiệu năng tải. Phím tắt: Ctrl/⌘ + ,': 'Google OAuth and download performance. Shortcut: Ctrl/⌘ + ,',
      'Tạo trong Google Cloud Console → Credentials → OAuth client ID → loại Desktop app, và bật Google Drive API.': 'Create it in Google Cloud Console → Credentials → OAuth client ID → type Desktop app, and enable the Google Drive API.',
      'HIỆU NĂNG': 'PERFORMANCE',
      'WORKER MỖI FILE (1–32)': 'WORKERS PER FILE (1–32)',
      'CHUNK (MiB)': 'CHUNK (MiB)',
      'RING BUFFER MỖI WORKER (MiB)': 'RING BUFFER PER WORKER (MiB)',
      'FILE TẢI SONG SONG (1–8)': 'PARALLEL FILES (1–8)',
      'RAM tối đa cho ring buffer ≈ worker × ring × số file song song.': 'Max ring-buffer RAM ≈ workers × ring × parallel files.',
      'QUOTA & TOÀN VẸN': 'QUOTA & INTEGRITY',
      'Khi file hết quota tải: copy vào Drive cá nhân rồi tải bản copy (cần dung lượng trống trong Drive của bạn)': 'When a file hits its download quota: copy it into your own Drive and download the copy (needs free space in your Drive)',
      'Giữ lại bản copy trong Drive sau khi tải xong (mặc định: xoá)': 'Keep the copy in Drive after downloading (default: delete)',
      'Kiểm tra MD5 sau khi tải': 'Verify MD5 after download',
      'Huỷ': 'Cancel',
      'Lưu': 'Save',
      'Đã lưu cài đặt': 'Settings saved',
    },
    zh: {
      'Phiên bản ứng dụng của bạn đã cũ. Vui lòng cập nhật lên phiên bản mới nhất để tiếp tục sử dụng.': '您的应用版本已过旧，请更新到最新版本后继续使用。',
      'Cập nhật ngay': '立即更新',
      'Cài đặt peaDrive': 'peaDrive 设置',
      'Hiệu năng tải và tuỳ chọn cá nhân. Phím tắt: Ctrl/⌘ + ,': '下载性能与个人选项。快捷键：Ctrl/⌘ + ,',
      'Google OAuth và hiệu năng tải. Phím tắt: Ctrl/⌘ + ,': 'Google OAuth 与下载性能。快捷键：Ctrl/⌘ + ,',
      'Tạo trong Google Cloud Console → Credentials → OAuth client ID → loại Desktop app, và bật Google Drive API.': '在 Google Cloud Console → 凭据 → OAuth 客户端 ID 中创建，类型选“桌面应用”，并启用 Google Drive API。',
      'HIỆU NĂNG': '性能',
      'WORKER MỖI FILE (1–32)': '每个文件的线程数 (1–32)',
      'CHUNK (MiB)': '分块 (MiB)',
      'RING BUFFER MỖI WORKER (MiB)': '每线程环形缓冲区 (MiB)',
      'FILE TẢI SONG SONG (1–8)': '并行文件数 (1–8)',
      'RAM tối đa cho ring buffer ≈ worker × ring × số file song song.': '环形缓冲区最大内存 ≈ 线程数 × 缓冲区 × 并行文件数。',
      'QUOTA & TOÀN VẸN': '配额与完整性',
      'Khi file hết quota tải: copy vào Drive cá nhân rồi tải bản copy (cần dung lượng trống trong Drive của bạn)': '文件下载配额用尽时：复制到你自己的 Drive 再下载副本（需要 Drive 有可用空间）',
      'Giữ lại bản copy trong Drive sau khi tải xong (mặc định: xoá)': '下载完成后保留 Drive 中的副本（默认删除）',
      'Kiểm tra MD5 sau khi tải': '下载后校验 MD5',
      'Huỷ': '取消',
      'Lưu': '保存',
      'Đã lưu cài đặt': '设置已保存',
    },
  };
  const t = (vi) => (TXT[(document.documentElement.lang || 'vi').slice(0, 2)] || {})[vi] || vi;

  /* ---------- Chặn hoàn toàn khi phiên bản quá cũ ---------- */
  let blockEl = null;

  function showBlock(info) {
    if (blockEl) return;
    const url = info && info.url; // thiếu (vd. bị chặn giữa chừng) → hỏi backend lúc bấm nút; địa chỉ chỉ khai báo MỘT chỗ (config.rs)
    const css = document.createElement('style');
    css.textContent =
      '#pdBlock{position:fixed;inset:0;z-index:2147483647;display:grid;place-items:center;padding:16px;background:rgba(8,10,9,.55);' +
      '-webkit-backdrop-filter:blur(12px) saturate(.8);backdrop-filter:blur(12px) saturate(.8);' +
      'font:14px/1.5 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;color:var(--text,#e9eee9)}' +
      '#pdBlock .pd-card{width:min(440px,100%);box-sizing:border-box;padding:28px 26px 24px;text-align:center;background:var(--panel,#171c19);' +
      'border:1px solid rgba(255,255,255,.12);border-radius:16px;box-shadow:0 24px 70px rgba(0,0,0,.55)}' +
      '#pdBlock .pd-ic{width:52px;height:52px;margin:0 auto 14px;border-radius:50%;display:grid;place-items:center;background:var(--accent,#e8c15a);color:var(--on-accent,#1a1608)}' +
      '#pdBlock p{margin:0 0 22px;font-size:15px;line-height:1.6}' +
      '#pdBlock button{font:inherit;font-weight:700;cursor:pointer;border:0;border-radius:10px;padding:11px 28px;background:var(--accent,#e8c15a);color:var(--on-accent,#1a1608)}' +
      '#pdBlock button:focus-visible{outline:2px solid var(--text,#fff);outline-offset:3px}';
    const icon = h('div', { class: 'pd-ic' });
    icon.innerHTML = '<svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="M12 19V5"/><path d="m5 12 7-7 7 7"/></svg>';
    const btn = h('button', { type: 'button', text: t('Cập nhật ngay') });
    btn.addEventListener('click', async () => {
      try {
        // Liên kết tải do backend quyết định; `/api/status` vẫn được phép khi app đang bị chặn.
        const target = url || ((await rpc('GET', '/api/status')) || {}).update_url;
        if (target) await rpc('POST', '/api/update/open?url=' + encodeURIComponent(target));
      } catch (e) { console.error(e); }
    });
    const ov = h('div', { id: 'pdBlock', role: 'alertdialog', 'aria-modal': 'true', 'aria-labelledby': 'pdBlockMsg' },
      h('div', { class: 'pd-card' }, icon, h('p', { id: 'pdBlockMsg', text: t('Phiên bản ứng dụng của bạn đã cũ. Vui lòng cập nhật lên phiên bản mới nhất để tiếp tục sử dụng.') }), btn));
    blockEl = ov;
    document.documentElement.append(css, ov);

    // Không thể tắt/ẩn: mọi thứ bên dưới bị vô hiệu, phím tắt / dán / thả file bị chặn, Esc và Tab không thoát được.
    for (const el of [...document.body.children]) { el.inert = true; el.setAttribute('aria-hidden', 'true'); }
    const eat = (e) => { if (!ov.contains(e.target)) { e.preventDefault(); e.stopImmediatePropagation(); } };
    ['keyup', 'keypress', 'paste', 'copy', 'cut', 'drop', 'dragover', 'dragstart', 'contextmenu'].forEach((ev) => window.addEventListener(ev, eat, true));
    window.addEventListener('keydown', (e) => {
      if (e.key === 'Tab' || e.key === 'Escape') { e.preventDefault(); e.stopImmediatePropagation(); if (e.key === 'Tab') btn.focus(); return; }
      eat(e);
    }, true);
    new MutationObserver(() => { if (!ov.isConnected) document.documentElement.append(ov); }).observe(document.documentElement, { childList: true });
    document.querySelectorAll('.overlay, .menu, .acc-dd').forEach((el) => el.remove());
    btn.focus();
  }
  window.showForceUpdate = showBlock;

  // Vừa khởi động là kiểm tra phiên bản (backend cũng tự hỏi máy chủ ngay khi app mở).
  const startupCheck = () => rpc('GET', '/api/update/check').then((d) => { if (d && d.force) showBlock(d); }).catch(() => {});
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', startupCheck); else startupCheck();

  /* ---------- Cài đặt ---------- */
  const h = (tag, props, ...kids) => {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(props || {})) {
      if (k === 'class') el.className = v;
      else if (k === 'text') el.textContent = v;
      else if (k === 'style') Object.assign(el.style, v);
      else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
      else el.setAttribute(k, v);
    }
    for (const kid of kids.flat()) if (kid != null) el.append(kid);
    return el;
  };

  async function openSettings() {
    if (document.querySelector('.overlay.pd-settings')) return;
    let data;
    try { data = await rpc('GET', '/api/settings'); } catch (e) { return notify(String(e), true); }
    const st = data.settings;

    const field = (label, input) => h('div', {}, h('div', { class: 'lbl', text: label }), input);
    const num = (key, min, max) => h('input', { type: 'number', min, max, value: st[key], 'data-k': key });
    const check = (key, text) => {
      const cb = h('input', { type: 'checkbox', 'data-k': key, style: { width: 'auto', margin: '3px 9px 0 0' } });
      cb.checked = !!st[key];
      return h('label', { style: { display: 'flex', alignItems: 'flex-start', margin: '8px 0', cursor: 'pointer' } }, cb, h('span', { text, style: { color: 'var(--text-2)' } }));
    };

    // Build đã nhúng sẵn Client ID (bản phát hành): người dùng cuối không phải (và không cần) nhập API → ẩn ô nhập.
    // Build dev không nhúng thì vẫn hiện để dán Client ID vào.
    const embedded = !!data.oauth.embedded;
    const id = embedded ? null : h('input', { value: data.oauth.client_id, placeholder: 'xxxxxxxx.apps.googleusercontent.com', spellcheck: 'false', autocomplete: 'off' });
    const secret = embedded ? null : h('input', { type: 'password', value: data.oauth.client_secret, spellcheck: 'false', autocomplete: 'off' });
    const grid = h('div', { style: { display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '0 14px' } },
      field(t('WORKER MỖI FILE (1–32)'), num('workers', 1, 32)),
      field(t('CHUNK (MiB)'), num('chunkMib', 1, 128)),
      field(t('RING BUFFER MỖI WORKER (MiB)'), num('ringMib', 1, 128)),
      field(t('FILE TẢI SONG SONG (1–8)'), num('parallelFiles', 1, 8)));

    const close = () => ov.remove();
    const save = async () => {
      const settings = { ...st };
      grid.querySelectorAll('input[data-k]').forEach((i) => { settings[i.dataset.k] = Number(i.value) || st[i.dataset.k]; });
      ov.querySelectorAll('input[type=checkbox][data-k]').forEach((i) => { settings[i.dataset.k] = i.checked; });
      try {
        await rpc('POST', '/api/settings', embedded ? { settings } : { oauth: { client_id: id.value, client_secret: secret.value }, settings });
        close();
        notify(t('Đã lưu cài đặt'));
      } catch (e) { err.textContent = String(e); err.hidden = false; }
    };

    const err = h('div', { class: 'err', hidden: '' });
    const ov = h('div', { class: 'overlay pd-settings' },
      h('div', { class: 'dialog link-dlg', style: { maxHeight: 'calc(100vh - 40px)', overflow: 'auto' } },
        h('div', { class: 'head' }, h('div', {}, h('h3', { text: t('Cài đặt peaDrive') }), h('p', { text: embedded ? t('Hiệu năng tải và tuỳ chọn cá nhân. Phím tắt: Ctrl/⌘ + ,') : t('Google OAuth và hiệu năng tải. Phím tắt: Ctrl/⌘ + ,') }))),
        embedded ? null : field('GOOGLE OAUTH — CLIENT ID (DESKTOP APP)', id),
        embedded ? null : field('CLIENT SECRET', secret),
        embedded ? null : h('p', { style: { fontSize: '10.5px', marginTop: '6px' }, text: t('Tạo trong Google Cloud Console → Credentials → OAuth client ID → loại Desktop app, và bật Google Drive API.') }),
        h('div', { class: 'lbl', text: t('HIỆU NĂNG') }), grid,
        h('p', { style: { fontSize: '10.5px', margin: '2px 0 4px' }, text: t('RAM tối đa cho ring buffer ≈ worker × ring × số file song song.') }),
        h('div', { class: 'lbl', text: t('QUOTA & TOÀN VẸN') }),
        check('copyFallback', t('Khi file hết quota tải: copy vào Drive cá nhân rồi tải bản copy (cần dung lượng trống trong Drive của bạn)')),
        check('keepCopy', t('Giữ lại bản copy trong Drive sau khi tải xong (mặc định: xoá)')),
        check('verifyMd5', t('Kiểm tra MD5 sau khi tải')),
        err,
        h('div', { class: 'acts' }, h('button', { class: 'ghost', text: t('Huỷ'), onclick: close }), h('button', { class: 'primary', text: t('Lưu'), onclick: save }))));
    ov.addEventListener('mousedown', (e) => { if (e.target === ov) close(); });
    ov.addEventListener('keydown', (e) => { if (e.key === 'Escape') close(); });
    document.body.appendChild(ov);
    (embedded || data.oauth.configured ? ov.querySelector('input[data-k]') : id).focus();
  }

  document.addEventListener('keydown', (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key === ',') { e.preventDefault(); openSettings(); }
  });
  window.peadriveSettings = openSettings;
})();
