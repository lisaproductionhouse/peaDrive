// Thử giao diện thật (ui/index.html) cùng cầu nối thật (src-tauri/src/bridge.js) trong jsdom.
// Backend giả trả JSON do chính Rust (`Api::call`) sinh ra (tools/ui-smoke/fixtures, xuất từ test `ui_api_contract`).
//   npm install && npm test
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { JSDOM, VirtualConsole } from 'jsdom';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../..');
// Giao diện đã tách thành index.html + style.css + app.js. jsdom nạp trang từ chuỗi nên không tự tải được file ngoài:
// nhúng nội dung hai file đó vào đúng chỗ của thẻ <link>/<script src> (cùng thứ tự như trình duyệt thật).
const readUi = (f) => fs.readFileSync(path.join(root, 'ui', f), 'utf8');
const LINK_TAG = '<link rel="stylesheet" href="style.css">', SCRIPT_TAG = '<script src="app.js"></script>';
const html = (() => {
  const src = readUi('index.html');
  assert.ok(src.includes(LINK_TAG), 'index.html phải nạp style.css');
  assert.ok(src.includes(SCRIPT_TAG), 'index.html phải nạp app.js (không phải script.js)');
  return src
    .replace(LINK_TAG, () => `<style>${readUi('style.css')}</style>`)
    .replace(SCRIPT_TAG, () => `<script>${readUi('app.js').replace(/<\/script/gi, '<\\/script')}</script>`);
})();
const bridge = fs.readFileSync(path.join(root, 'src-tauri/src/bridge.js'), 'utf8');
const fx = (n) => JSON.parse(fs.readFileSync(path.join(here, 'fixtures', `${n}.json`), 'utf8'));

const UPDATE_URL = 'https://www.peamark.com/app-api/download.php?app=peadrive&os=win';
const OUTDATED = 'Phiên bản ứng dụng của bạn đã cũ. Vui lòng cập nhật lên phiên bản mới nhất để tiếp tục sử dụng.';
const SETTINGS = { workers: 8, chunkMib: 16, ringMib: 8, parallelFiles: 2, maxRetries: 8, verifyMd5: false, copyFallback: true, keepCopy: false, autoResume: false };
const ME = { id: 'me@example.com', email: 'me@example.com', name: 'Me' };
const OTHER = { id: 'other@example.com', email: 'other@example.com', name: 'Người khác' };
const MT = '2026-10-09T02:12:08Z';
const LOCAL_ITEM = (name, is_dir = false) => ({ name, path: `/media/render/${name}`, is_dir, size: is_dir ? 0 : 1000, mod_time: MT });
const LOCAL_LIST = {
  current_path: '/media/render', parent_path: '/media', free: 5_000_000_000,
  items: [LOCAL_ITEM('Clips', true), LOCAL_ITEM('Photos', true), LOCAL_ITEM('DSC00123.ARW'), LOCAL_ITEM('DSC00123.JPG'), LOCAL_ITEM('final.mp4')],
};
const WATCHER = { id: '7', source: '/media/render/Clips', destination: 'Drive của tôi › Clips', extensions: ['mp4', 'mov'], uploaded: 2, skipped: 5, queued: 2, waiting: 1 };
const LINK_FOLDER = { id: 'LINKFOLDER_1234567890', name: 'Thư mục chia sẻ', is_dir: true, size: 0, modified_time: null, is_google_doc: false, can_download: true, web_view_link: 'https://drive.google.com/drive/folders/LINKFOLDER_1234567890', resource_key: '0-KEYabc' };
const LINK_FILE = { id: 'LINKFILE_1234567890ab', name: 'bao-cao.pdf', is_dir: false, size: 2048, modified_time: '2026-01-01T00:00:00Z', is_google_doc: false, can_download: true, web_view_link: 'https://drive.google.com/file/d/LINKFILE_1234567890ab/view', resource_key: null };

/** Backend giả có trạng thái, bắt chước đúng hợp đồng của `peadrive_core::api::Api`. */
function makeBackend(opts = {}) {
  const calls = [];
  let closed = false;
  const st = {
    connected: opts.connected ?? true,
    oauthConfigured: opts.oauthConfigured ?? true,
    accounts: opts.accounts ?? [ME],
    active: opts.active === undefined ? ME.id : opts.active,
    signins: 0,
    session: opts.session ?? fx('session'),
    force: opts.force ?? false,
    relogin: opts.relogin ?? false,
    linkRoot: opts.linkRoot ?? LINK_FOLDER,
    linkError: opts.linkError ?? null,
    updateInfo: opts.updateInfo ?? null,
    local: opts.local ?? null,
    watchers: opts.watchers ?? [],
    ready: opts.ready ?? true,            // trạng thái sẵn sàng backend báo cho chấm ở thanh dưới
    readyReason: opts.readyReason ?? '',
    queueDown: false,                     // true = gọi hàng đợi thất bại (mô phỏng mất liên lạc với backend)
    oauthEmbedded: opts.oauthEmbedded ?? false,   // true = bản build đã nhúng sẵn Client ID (Cài đặt ẩn ô nhập OAuth)
  };
  async function invoke(cmd, args) {
    assert.equal(cmd, 'api', 'chỉ có đúng một lệnh Tauri: api');
    const { method, url, body } = args;
    calls.push({ method, url, body });
    // Sau khi cửa sổ thử nghiệm đóng, các lời gọi còn dang dở treo vĩnh viễn (không chạy tiếp vào DOM đã huỷ).
    await new Promise((r) => setTimeout(r, 3));
    if (closed) return new Promise(() => {});
    const u = new URL('http://x' + url);
    const route = `${method} ${u.pathname}`;
    if (st.force && !['GET /api/status', 'GET /api/update/check', 'POST /api/update/open', 'POST /api/open'].includes(route)) throw `E_OUTDATED:${OUTDATED}`;
    const active = st.accounts.find((a) => a.id === st.active) || null;
    switch (route) {
      case 'GET /api/status': return { ...fx('status'), connected: st.connected && !!active, oauth_configured: st.oauthConfigured, login_error: null, ready: st.ready, ready_reason: st.readyReason, accounts: st.accounts, active: st.active, account: active, signins: st.signins, blocked: st.force, update_url: UPDATE_URL };
      case 'GET /api/accounts': return { active: st.active, accounts: st.accounts };
      case 'POST /api/accounts/switch': {
        if (st.relogin) return { ok: false, relogin: true };
        st.active = u.searchParams.get('id');
        return { ok: true, account: st.accounts.find((a) => a.id === st.active) };
      }
      case 'POST /api/accounts/remove': {
        const id = u.searchParams.get('id');
        st.accounts = st.accounts.filter((a) => a.id !== id);
        if (st.active === id) st.active = st.accounts[0]?.id ?? null;
        return { ok: true, active: st.active, accounts: st.accounts };
      }
      case 'POST /api/login':
        if (!st.oauthConfigured) throw 'E_OAUTH_CONFIG:Chưa có Google OAuth Client ID (loại Desktop app).';
        // "Trình duyệt" đăng nhập xong sau ~0,5 giây: thêm tài khoản mới và chuyển sang nó.
        setTimeout(() => { if (!closed) { st.accounts = [...st.accounts, { id: 'new@example.com', email: 'new@example.com', name: 'Mới' }]; st.active = 'new@example.com'; st.signins++; } }, 500);
        return { ok: true };
      case 'GET /api/drive/about': return { ...fx('about'), email: st.active, name: active?.name || '' };
      case 'GET /api/drive/list': return opts.driveList ?? fx('drive_list');
      case 'GET /api/drive/link':
        if (st.linkError) throw st.linkError;
        return st.linkRoot;
      case 'GET /api/local/list': return u.searchParams.get('path') === '::drives' ? fx('computer') : (st.local ?? fx('local_list'));
      case 'GET /api/transfer/queue':
        if (st.queueDown) throw 'backend không trả lời';
        return { ...(opts.queue ? opts.queue() : fx('queue')), smart: st.watchers.length, ready: st.ready, ready_reason: st.readyReason };
      case 'GET /api/transfer/smart-upload': return { items: st.watchers };
      case 'POST /api/transfer/smart-upload':
        st.watchers = body.paths.map((p, i) => ({ ...WATCHER, id: String(10 + i), source: p, extensions: body.extensions }));
        return { started: body.paths.length, items: st.watchers, warnings: [] };
      case 'POST /api/transfer/smart-upload/stop': {
        const id = u.searchParams.get('id');
        const n = st.watchers.length;
        st.watchers = id ? st.watchers.filter((w) => w.id !== id) : [];
        return { ok: true, stopped: n - st.watchers.length };
      }
      case 'POST /api/transfer/upload':
        return opts.upload ? opts.upload(body) : { queued: (body.paths || []).length, skipped: 0, folders: 0, destination: 'Drive của tôi', warnings: [] };
      case 'GET /api/session': return st.session;
      case 'GET /api/update/check':
        return st.updateInfo ?? { available: st.force, force: st.force, version: '1.0.9', current: 'v0.0.1', min_required: '1.0.9', status: st.force ? 'force_update' : 'up_to_date', url: UPDATE_URL, notes: '' };
      case 'POST /api/transfer/download': return { added: (body.items || []).length, warnings: [] };
      case 'POST /api/session': return { ok: true };
      case 'POST /api/open':
      case 'POST /api/update/open': return { ok: true };
      case 'GET /api/settings': return { oauth: { client_id: '', client_secret: '', configured: st.oauthConfigured, embedded: st.oauthEmbedded }, settings: SETTINGS, data_dir: '/tmp' };
      case 'POST /api/settings': return { ok: true };
      default: throw `Không có API ${route}`;
    }
  }
  return { invoke, calls, st, shutdown: () => { closed = true; } };
}

async function boot(backend, seed) {
  const errors = [];
  const vc = new VirtualConsole();
  vc.on('jsdomError', (e) => errors.push(`jsdomError: ${e.detail?.message || e.message}`));
  vc.on('error', (...a) => errors.push(`console.error: ${a.join(' ')}`));
  const dom = new JSDOM(html, {
    url: 'http://tauri.localhost/',
    runScripts: 'dangerously',
    pretendToBeVisual: true,
    virtualConsole: vc,
    beforeParse(window) {
      window.addEventListener('error', (e) => errors.push(`window.onerror: ${e.message}`));
      window.addEventListener('unhandledrejection', (e) => errors.push(`unhandledrejection: ${e.reason}`));
      window.__TAURI__ = { core: { invoke: backend.invoke } };
      // jsdom không có fetch/Response; WebView thật thì có.
      window.fetch = async () => { throw new Error('mạng bị tắt trong thử nghiệm'); };
      window.Response = globalThis.Response;
      window.TextDecoder ||= globalThis.TextDecoder;
      seed?.(window);
      window.matchMedia ||= () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
      window.HTMLElement.prototype.scrollIntoView = function () {};
      Object.defineProperty(window.navigator, 'clipboard', { value: { writeText: async () => {} }, configurable: true });
      window.eval(bridge); // giống initialization_script của Tauri: chạy trước script của trang
    },
  });
  return { window: dom.window, document: dom.window.document, errors, close: () => { backend.shutdown(); dom.window.close(); } };
}

async function until(fn, what, ms = 6000) {
  const t0 = Date.now();
  for (;;) {
    try { const v = fn(); if (v) return v; } catch (_) { /* thử lại */ }
    if (Date.now() - t0 > ms) throw new Error(`quá thời gian chờ: ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const hasCall = (b, method, url) => b.calls.some((c) => c.method === method && c.url === url);
const callsTo = (b, prefix) => b.calls.filter((c) => c.url.startsWith(prefix));
const rows = (app) => app.document.querySelectorAll('#driveList .row');

const tests = [];
const test = (name, fn) => tests.push([name, fn]);

// ───────────────────────── nền tảng ─────────────────────────
test('khởi động khi đã đăng nhập: hiển thị tài khoản, Drive, máy, hàng đợi', async () => {
  const app = await boot(makeBackend());
  try {
    await until(() => rows(app).length === 5, '5 dòng Drive');
    await until(() => app.document.querySelectorAll('#localList .row').length >= 1, 'danh sách máy');
    assert.equal(app.document.getElementById('accEmail').textContent.trim(), 'me@example.com');
    assert.match(app.document.getElementById('sideVersion').textContent, /9\.9\.9/);
    assert.match(app.document.getElementById('driveList').textContent, /Tài liệu/);
    await until(() => /big\.bin/.test(app.document.getElementById('qList').textContent), 'dòng hàng đợi big.bin');
    assert.match(app.document.getElementById('sbQuota').textContent, /Drive/);
    assert.deepEqual(app.errors, [], 'không được có lỗi JS');
  } finally { app.close(); }
});

test('thanh bên: Có gắn dấu sao / Thùng rác / Bộ nhớ gọi đúng /api/drive/list', async () => {
  const b = makeBackend();
  const app = await boot(b);
  try {
    await until(() => rows(app).length === 5, 'Drive đã tải');
    for (const loc of ['starred', 'trash', 'storage', 'shared']) {
      app.document.querySelector(`.side-btn[data-loc="${loc}"]`).click();
      await until(() => hasCall(b, 'GET', `/api/drive/list?location=${loc}&folder_id=root`), `list ${loc}`);
    }
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('tải xuống: POST /api/transfer/download đúng thư mục đích và ID', async () => {
  const b = makeBackend();
  const app = await boot(b);
  try {
    await until(() => rows(app).length === 5, 'Drive đã tải');
    const item = fx('drive_list').items[1];
    await app.window.downloadSelected([item]);
    const call = b.calls.find((c) => c.method === 'POST' && c.url === '/api/transfer/download');
    assert.ok(call, 'phải gọi /api/transfer/download');
    assert.equal(call.body.dest_dir, fx('local_list').current_path);
    assert.equal(call.body.items[0].id, item.id);
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('quyền drive.file: mục không do peaDrive tạo bị làm mờ Đổi tên / Thùng rác / Xoá / Chia sẻ, vẫn tải xuống được', async () => {
  const base = fx('drive_list');
  const items = base.items.map((it, i) => ({ ...it, can_modify: i !== 1 })); // mục số 1 (big.bin) không do app tạo
  const app = await boot(makeBackend({ driveList: { ...base, items } }));
  try {
    await until(() => rows(app).length === 5, 'Drive đã tải');
    // UI sắp xếp danh sách nên chọn hàng theo TÊN, không theo thứ tự trả về của API.
    const menuOf = async (name) => {
      const row = [...rows(app)].find((r) => r.textContent.includes(name));
      assert.ok(row, `không thấy dòng "${name}"`);
      row.dispatchEvent(new app.window.MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 40, clientY: 40 }));
      const menu = await until(() => app.document.querySelector('.menu'), 'menu chuột phải');
      return Object.fromEntries([...menu.querySelectorAll('button')].map((b) => [b.querySelector('span').textContent.trim(), b]));
    };
    const WRITE = ['Sao chép link chia sẻ', 'Đổi tên…', 'Chuyển vào thùng rác', 'Xoá vĩnh viễn…'];
    const theirs = await menuOf('big.bin');
    for (const label of WRITE) {
      assert.ok(theirs[label], `thiếu mục menu "${label}"`);
      assert.equal(theirs[label].disabled, true, `"${label}" phải bị làm mờ với mục không do app tạo`);
      assert.match(theirs[label].title, /do chính nó tạo/, 'phải có lời giải thích');
    }
    assert.equal(theirs['Tải xuống'].disabled, false, 'tải xuống vẫn dùng được (drive.readonly)');
    const mine = await menuOf('Tài liệu');
    for (const label of WRITE) {
      assert.equal(mine[label].disabled, false, `"${label}" dùng được với mục do app tạo`);
      assert.equal(mine[label].title, '');
    }
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

// ───────────────────────── Tải lên thủ công · thông báo quyền riêng tư ─────────────────────────
const uploadCalls = (b) => b.calls.filter((c) => c.method === 'POST' && c.url === '/api/transfer/upload');
const toastText = (app) => app.document.getElementById('toastText').textContent;
const plain = (o) => JSON.parse(JSON.stringify(o));   // đối tượng tạo trong cửa sổ jsdom khác "realm" với Node: so sánh qua JSON thuần
const localRow = (app, name) => [...localRows(app)].find((r) => r.textContent.includes(name));
const pathOf = (name) => LOCAL_LIST.items.find((i) => i.name === name).path;
const PRIVACY = 'https://peamark.com/privacy';
const FALLBACK_DEST = 'Drive của tôi › peaDrive Uploads › Tài liệu';
const askFirst = (body) => body.use_fallback
  ? { queued: 1, skipped: 0, folders: 0, destination: FALLBACK_DEST, warnings: [] }
  : { needs_fallback: true, blocked_name: 'Tài liệu', destination: FALLBACK_DEST, queued: 0, skipped: 0, folders: 0, warnings: [] };

test('Tải lên thủ công: gửi đúng thư mục đích và đường dẫn đã chọn, rồi báo kết quả', async () => {
  const b = makeBackend({ local: LOCAL_LIST, upload: () => ({ queued: 2, skipped: 1, folders: 1, destination: 'Drive của tôi', warnings: [] }) });
  const app = await boot(b);
  try {
    await until(() => localRows(app).length === 5 && rows(app).length === 5, 'hai khung đã tải');
    const btn = app.document.getElementById('btnUpload');
    assert.equal(btn.disabled, true, 'chưa chọn gì → tắt');
    localRow(app, 'final.mp4').click();
    assert.equal(btn.disabled, false);
    btn.click();
    await until(() => uploadCalls(b).length === 1, 'gọi /api/transfer/upload');
    assert.deepEqual(plain(uploadCalls(b)[0].body), { folder_id: 'root', paths: [pathOf('final.mp4')] });
    await until(() => toastText(app) === 'Đã thêm 2 file vào hàng đợi tải lên · 1 file đã có trên Drive', 'thông báo kết quả');
    assert.equal(app.document.getElementById('toast').classList.contains('err'), false);
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('Tải lên thủ công: cảnh báo của backend (vd. bỏ qua file rỗng) hiện bằng thông báo lỗi', async () => {
  const b = makeBackend({ local: LOCAL_LIST, upload: () => ({ queued: 1, skipped: 0, folders: 0, destination: 'Drive của tôi', warnings: ['Bỏ qua 1 file rỗng (0 byte)'] }) });
  const app = await boot(b);
  try {
    await until(() => localRows(app).length === 5 && rows(app).length === 5, 'hai khung đã tải');
    localRow(app, 'final.mp4').click();
    app.document.getElementById('btnUpload').click();
    await until(() => toastText(app) === 'Bỏ qua 1 file rỗng (0 byte)', 'hiện cảnh báo');
    assert.equal(app.document.getElementById('toast').classList.contains('err'), true);
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('Tải lên thủ công vào thư mục không do peaDrive tạo: hỏi trước, đồng ý thì gọi lại với use_fallback', async () => {
  const b = makeBackend({ local: LOCAL_LIST, upload: askFirst });
  const app = await boot(b);
  try {
    await until(() => localRows(app).length === 5 && rows(app).length === 5, 'hai khung đã tải');
    localRow(app, 'final.mp4').click();
    app.document.getElementById('btnUpload').click();
    const dlg = await until(() => app.document.querySelector('.dialog'), 'hộp thoại hỏi');
    assert.match(dlg.textContent, /Thư mục này không do peaDrive tạo/);
    assert.match(dlg.textContent, /“Tài liệu”/, 'nêu thư mục bị từ chối');
    assert.ok(dlg.textContent.includes(FALLBACK_DEST), 'nêu rõ nơi sẽ tải lên');
    assert.equal(uploadCalls(b).length, 1, 'chưa tải gì khi chưa đồng ý');
    dlg.querySelector('.acts button.primary').click();
    await until(() => uploadCalls(b).length === 2, 'gọi lại sau khi đồng ý');
    assert.deepEqual(plain(uploadCalls(b)[1].body), { folder_id: 'root', paths: [pathOf('final.mp4')], use_fallback: true });
    await until(() => toastText(app) === 'Đã thêm 1 file vào hàng đợi tải lên', 'thông báo kết quả');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('Tải lên thủ công: bấm Huỷ ở hộp thoại hỏi thì không tải gì', async () => {
  const b = makeBackend({ local: LOCAL_LIST, upload: askFirst });
  const app = await boot(b);
  try {
    await until(() => localRows(app).length === 5 && rows(app).length === 5, 'hai khung đã tải');
    localRow(app, 'final.mp4').click();
    app.document.getElementById('btnUpload').click();
    const dlg = await until(() => app.document.querySelector('.dialog'), 'hộp thoại hỏi');
    dlg.querySelector('.acts .ghost').click();
    await sleep(150);
    assert.equal(uploadCalls(b).length, 1, 'không gọi lại khi người dùng huỷ');
    assert.equal(app.document.querySelector('.dialog'), null, 'hộp thoại đã đóng');
    // Lần sau vẫn tải được bình thường (cờ "đang chuẩn bị" đã được nhả).
    app.document.getElementById('btnUpload').click();
    await until(() => uploadCalls(b).length === 2, 'bấm lại được');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('Tải lên thủ công: hộp thoại hỏi được dịch sang tiếng Anh', async () => {
  const b = makeBackend({ local: LOCAL_LIST, upload: askFirst });
  const app = await boot(b, (w) => w.localStorage.setItem('peadrive.lang', 'en'));
  try {
    await until(() => localRows(app).length === 5 && rows(app).length === 5, 'hai khung đã tải');
    localRow(app, 'final.mp4').click();
    app.document.getElementById('btnUpload').click();
    const dlg = await until(() => app.document.querySelector('.dialog'), 'hộp thoại hỏi');
    await until(() => /cannot upload straight into “Tài liệu”/.test(dlg.textContent), 'bản dịch có chỗ trống {}');
    assert.match(dlg.textContent, /This folder was not created by peaDrive/);
    assert.ok(dlg.textContent.includes(`Upload to “${FALLBACK_DEST}” instead?`), dlg.textContent);
    assert.match(dlg.querySelector('.acts button.primary').textContent, /Upload to the replacement folder/);
  } finally { app.close(); }
});

test('Quyền riêng tư: màn hình đăng nhập có thông báo + liên kết; bấm link mở bằng trình duyệt qua cầu nối', async () => {
  const b = makeBackend({ connected: false });
  const app = await boot(b);
  try {
    const note = await until(() => app.document.querySelector('.empty .priv-note'), 'thông báo quyền riêng tư ở màn hình đăng nhập');
    assert.match(note.textContent, /không gửi dữ liệu Drive tới máy chủ nào của peaDrive/);
    assert.match(note.textContent, /Google API Services User Data Policy/);
    assert.match(note.textContent, /Limited Use/);
    const link = note.querySelector('a.priv-link');
    assert.equal(link.getAttribute('href'), PRIVACY);
    // Bấm link: KHÔNG điều hướng cửa sổ app mà mở trình duyệt mặc định qua /api/open.
    const ev = new app.window.MouseEvent('click', { bubbles: true, cancelable: true });
    link.dispatchEvent(ev);
    assert.equal(ev.defaultPrevented, true, 'không để WebView điều hướng sang trang ngoài');
    await until(() => hasCall(b, 'POST', '/api/open?url=' + encodeURIComponent(PRIVACY)), 'mở link bằng trình duyệt');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('Quyền riêng tư: link nằm trong hộp thoại Giới thiệu (bấm vào phiên bản), không còn ở thanh trạng thái', async () => {
  const b = makeBackend();
  const app = await boot(b);
  try {
    await until(() => rows(app).length === 5, 'Drive đã tải');
    assert.equal(app.document.getElementById('sbPrivacy'), null, 'đã bỏ link khỏi thanh trạng thái');
    assert.equal(app.document.querySelector('.statusbar a'), null, 'thanh trạng thái không còn liên kết nào');
    app.document.getElementById('sideVersion').click();          // bấm vào phiên bản → Giới thiệu
    const about = await until(() => app.document.querySelector('.about-dlg'), 'hộp thoại Giới thiệu');
    const link = about.querySelector(`a[href="${PRIVACY}"][data-ext]`);
    assert.ok(link, 'có link chính sách trong hộp thoại Giới thiệu');
    assert.ok([...about.querySelectorAll('dt')].some((dt) => dt.textContent.trim() === 'Quyền riêng tư'), 'có nhãn "Quyền riêng tư" cạnh link');
    const ev = new app.window.MouseEvent('click', { bubbles: true, cancelable: true });
    link.dispatchEvent(ev);
    assert.equal(ev.defaultPrevented, true, 'không để WebView điều hướng sang trang ngoài');
    await until(() => hasCall(b, 'POST', '/api/open?url=' + encodeURIComponent(PRIVACY)), 'mở link bằng trình duyệt');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('Quyền riêng tư: thông báo, link và nhãn trong Giới thiệu được dịch sang EN và ZH', async () => {
  for (const [lang, policy, linkText, aboutLabel] of [
    ['en', /peaDrive's use and transfer to any other app of information received from Google APIs will adhere to Google API Services User Data Policy, including the Limited Use requirements\./, 'Privacy Policy', 'Privacy'],
    ['zh', /Google API 服务用户数据政策/, '隐私政策', '隐私'],
  ]) {
    const app = await boot(makeBackend({ connected: false }), (w) => w.localStorage.setItem('peadrive.lang', lang));
    try {
      const note = await until(() => app.document.querySelector('.empty .priv-note'), `thông báo (${lang})`);
      await until(() => policy.test(note.textContent), `bản dịch ${lang}`);
      assert.equal(note.querySelector('a').textContent.trim(), linkText);
      app.document.getElementById('sideVersion').click();
      const about = await until(() => app.document.querySelector('.about-dlg'), `Giới thiệu (${lang})`);
      await until(() => [...about.querySelectorAll('dt')].some((dt) => dt.textContent.trim() === aboutLabel), `nhãn ${aboutLabel} trong Giới thiệu`);
    } finally { app.close(); }
  }
});

// ───────────────────────── Thanh trạng thái và tốc độ ─────────────────────────
const MB = 1024 * 1024;
const workingQueue = (speed) => ({
  paused: false, smart: 0,
  items: [{ id: '9', direction: 'upload', status: 'working', name: 'a.mp4', source: '/a.mp4', destination: 'Drive của tôi', total_bytes: 900 * MB, done_bytes: 300 * MB, speed, error_msg: '' }],
});
const SPEED_SPOTS = ['#sbText', '#qSummary', '#qList .q-row'];

test('Thanh trạng thái: chấm trạng thái luôn là phần tử ngoài cùng bên phải, kể cả khi đang truyền', async () => {
  const app = await boot(makeBackend({ queue: () => workingQueue(24.5 * MB) }));
  try {
    const bar = app.document.querySelector('.statusbar');
    assert.equal(bar.lastElementChild.id, 'sbDot', 'rảnh: chấm đứng cuối');
    assert.equal(bar.querySelectorAll('#sbDot').length, 1);
    await app.window.pollQueue();
    assert.match(app.document.getElementById('sbText').textContent, /24,5 MB\/s/, 'đang truyền: có chữ trạng thái kèm tốc độ');
    assert.equal(bar.lastElementChild.id, 'sbDot', 'đang truyền: chấm vẫn đứng cuối, chữ nằm bên trái chấm');
    assert.ok(app.document.getElementById('sbText').compareDocumentPosition(app.document.getElementById('sbDot')) & app.window.Node.DOCUMENT_POSITION_FOLLOWING, 'chữ đứng trước chấm');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('Tốc độ: khung cố định ở dòng trạng thái, tiêu đề và hàng đợi, đổi từ 1 sang nhiều chữ số không đổi cấu trúc', async () => {
  let speed = 3 * MB;
  const app = await boot(makeBackend({ queue: () => workingQueue(speed) }));
  try {
    const parts = (sel) => {
      const e = app.document.querySelector(`${sel} .spd`);
      assert.ok(e, `${sel} phải dùng khung .spd`);
      return [e.querySelector('.spd-n').textContent, e.querySelector('.spd-u').textContent, e.textContent];
    };
    const shape = () => SPEED_SPOTS.map((sel) => [...app.document.querySelector(`${sel} .spd`).children].map((c) => c.className));
    await app.window.pollQueue();
    for (const sel of SPEED_SPOTS) assert.deepEqual(parts(sel), ['3,0', 'MB/s', '3,0 MB/s'], sel);
    const before = JSON.stringify(shape());
    speed = 24.5 * MB;                       // 1 chữ số → nhiều chữ số
    await app.window.pollQueue();
    for (const sel of SPEED_SPOTS) assert.deepEqual(parts(sel), ['24,5', 'MB/s', '24,5 MB/s'], sel);
    assert.equal(JSON.stringify(shape()), before, 'khung giống hệt trước và sau khi đổi số chữ số');
    speed = 0.9 * MB;                        // đổi cả đơn vị: vẫn cùng cấu trúc
    await app.window.pollQueue();
    for (const sel of SPEED_SPOTS) assert.deepEqual(parts(sel).slice(0, 2), ['921,6', 'KB/s'], sel);
    assert.equal(JSON.stringify(shape()), before);
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('Tốc độ: câu trạng thái vẫn là một nút văn bản nguyên vẹn nên được dịch sang EN dù đang có tốc độ', async () => {
  const app = await boot(makeBackend({ queue: () => workingQueue(24.5 * MB) }), (w) => w.localStorage.setItem('peadrive.lang', 'en'));
  try {
    await app.window.pollQueue();
    const t = app.document.getElementById('sbText');
    await until(() => t.firstChild.textContent === 'Uploading (33%) · 1 files left', 'câu trạng thái được dịch (khoá có {}) dù đang có tốc độ');
    assert.match(t.textContent, /24,5 MB\/s$/, 'tốc độ vẫn ở cuối câu');
  } finally { app.close(); }
});

test('CSS: khung tốc độ cố định, chữ số đều nhau, chấm trạng thái không bị co hay đẩy', () => {
  const css = readUi('style.css');
  const rule = (sel) => {
    const esc = sel.replace(/[.*+?^${}()|[\]\\#]/g, '\\$&');
    const m = css.match(new RegExp(`(?:^|\\n)${esc}\\s*\\{([^}]*)\\}`));
    return m ? m[1] : '';
  };
  assert.match(rule('.spd'), /display:\s*inline-flex/);
  assert.match(rule('.spd'), /white-space:\s*nowrap/);
  assert.match(rule('.spd-n'), /min-width:\s*\d+(\.\d+)?ch/);
  assert.match(rule('.spd-n'), /text-align:\s*right/);
  assert.match(rule('.spd-u'), /min-width:\s*\d+(\.\d+)?ch/);
  assert.match(rule('.statusbar, #qSummary, .q-row'), /font-variant-numeric:\s*tabular-nums/);
  assert.match(rule('.statusbar .dot'), /flex-shrink:\s*0/);
  assert.match(rule('.statusbar #sbText, .statusbar #sbQuota'), /min-width:\s*0/);
});

test('phiên khôi phục: hộp thoại hiện ra và "tiếp tục" gọi /api/session?action=resume', async () => {
  const b = makeBackend({ session: { restored: 2, bytes_left: 5_000_000, auto_resume: false } });
  const app = await boot(b);
  try {
    const ov = await until(() => app.document.querySelector('.overlay'), 'hộp thoại phiên');
    assert.match(ov.textContent, /2 tác vụ chưa xong/);
    ov.querySelector('[data-a="resume"]').click();
    await until(() => hasCall(b, 'POST', '/api/session?action=resume'), 'resume');
  } finally { app.close(); }
});

test('chưa có OAuth Client ID: đăng nhập báo lỗi gọn + tự mở Cài đặt, lưu gửi đúng dữ liệu', async () => {
  const b = makeBackend({ connected: false, oauthConfigured: false, accounts: [], active: null });
  const app = await boot(b);
  try {
    await until(() => hasCall(b, 'GET', '/api/status'), 'status');
    app.document.getElementById('accountBtn').click();
    await until(() => hasCall(b, 'POST', '/api/login'), 'login');
    const ov = await until(() => app.document.querySelector('.overlay.pd-settings'), 'hộp thoại Cài đặt');
    const toast = app.document.getElementById('toastText').textContent;
    assert.match(toast, /Client ID/);
    assert.ok(!toast.includes('E_OAUTH_CONFIG'), 'mã lỗi nội bộ không được lộ ra UI');
    await until(() => ov.querySelector('input[placeholder*="googleusercontent"]'), 'ô Client ID');
    ov.querySelector('input[placeholder*="googleusercontent"]').value = 'abc.apps.googleusercontent.com';
    ov.querySelector('button.primary').click();
    await until(() => hasCall(b, 'POST', '/api/settings'), 'lưu cài đặt');
    const saved = b.calls.find((c) => c.method === 'POST' && c.url === '/api/settings').body;
    assert.equal(saved.oauth.client_id, 'abc.apps.googleusercontent.com');
    assert.equal(saved.settings.workers, 8);
  } finally { app.close(); }
});

test('chấm trạng thái: xanh khi sẵn sàng; đỏ kèm lý do khi backend báo chưa sẵn sàng hoặc không liên lạc được', async () => {
  const b = makeBackend();
  const app = await boot(b);
  try {
    const dot = app.document.getElementById('sbDot');
    await until(() => rows(app).length === 5 && dot.dataset.state === 'ready', 'sẵn sàng (xanh)');
    assert.ok(dot.classList.contains('ready') && !dot.classList.contains('not-ready'));
    assert.equal(dot.title, 'Sẵn sàng');

    b.st.ready = false; b.st.readyReason = 'offline';             // mất kết nối tới Google
    await until(() => dot.classList.contains('not-ready'), 'đỏ khi offline');
    assert.match(dot.title, /Không kết nối được tới Google Drive/);
    assert.equal(dot.getAttribute('aria-label'), dot.title);

    b.st.readyReason = 'not_signed_in';                           // lý do đổi theo backend dù vẫn đỏ
    await until(() => /Chưa đăng nhập Google Drive/.test(dot.title), 'lý do đổi theo backend');

    b.st.ready = true; b.st.readyReason = '';                     // có mạng / đăng nhập lại
    await until(() => dot.classList.contains('ready'), 'xanh trở lại');
    assert.equal(dot.title, 'Sẵn sàng');

    b.st.queueDown = true;                                         // không hỏi được backend
    await until(() => dot.classList.contains('not-ready') && /Không liên lạc được với ứng dụng/.test(dot.title), 'đỏ khi mất liên lạc với backend');
    b.st.queueDown = false;
    await until(() => dot.classList.contains('ready'), 'xanh khi backend trả lời lại');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('Ctrl+, mở Cài đặt; window.open chỉ mở https qua trình duyệt hệ điều hành', async () => {
  const b = makeBackend();
  const app = await boot(b);
  try {
    await until(() => hasCall(b, 'GET', '/api/status'), 'status');
    app.document.dispatchEvent(new app.window.KeyboardEvent('keydown', { key: ',', ctrlKey: true, bubbles: true }));
    await until(() => app.document.querySelector('.overlay.pd-settings'), 'Ctrl+,');
    assert.equal(app.window.open('https://drive.google.com/file/d/abc/view'), null);
    assert.equal(app.window.open('file:///etc/passwd'), null);
    await until(() => hasCall(b, 'POST', '/api/open?url=https%3A%2F%2Fdrive.google.com%2Ffile%2Fd%2Fabc%2Fview'), 'mở https');
    assert.ok(!b.calls.some((c) => c.url.includes('file%3A')), 'không được mở file://');
  } finally { app.close(); }
});

test('nút Cài đặt (bánh răng) mở bảng cài đặt bất cứ lúc nào, không cần lỗi thiếu API', async () => {
  const b = makeBackend(); // OAuth đã cấu hình đủ: không có gì tự mở bảng cài đặt
  const app = await boot(b);
  try {
    await until(() => rows(app).length === 5, 'Drive đã tải');
    assert.equal(app.document.querySelector('.overlay.pd-settings'), null, 'không tự mở khi OAuth đã đủ');
    const btn = app.document.getElementById('btnSettings');
    assert.match(btn.title, /Ctrl/, 'tooltip nhắc phím tắt');
    btn.click();
    await until(() => app.document.querySelector('.overlay.pd-settings'), 'bảng cài đặt');
    assert.ok(hasCall(b, 'GET', '/api/settings'), 'bảng cài đặt nạp cấu hình từ backend');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('build đã nhúng sẵn Client ID: Cài đặt không hiện ô nhập OAuth, lưu chỉ gửi phần hiệu năng', async () => {
  const b = makeBackend({ oauthEmbedded: true });
  const app = await boot(b);
  try {
    await until(() => rows(app).length === 5, 'Drive đã tải');
    app.document.getElementById('btnSettings').click();
    const ov = await until(() => app.document.querySelector('.overlay.pd-settings'), 'bảng cài đặt');
    assert.equal(ov.querySelectorAll('input[type=password]').length, 0, 'không có ô Client Secret');
    assert.equal(ov.querySelector('input[placeholder*="googleusercontent"]'), null, 'không có ô Client ID');
    assert.ok(!/GOOGLE OAUTH/.test(ov.textContent), 'không có phần Google OAuth');
    assert.ok(ov.querySelector('input[data-k]'), 'vẫn còn phần hiệu năng tải');
    ov.querySelector('button.primary').click();
    await until(() => hasCall(b, 'POST', '/api/settings'), 'lưu cài đặt');
    const saved = b.calls.find((c) => c.method === 'POST' && c.url === '/api/settings').body;
    assert.ok(!('oauth' in saved), 'không gửi thông số OAuth khi build đã nhúng sẵn');
    assert.equal(saved.settings.workers, 8);
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('thanh quota ở sidebar hiển thị dung lượng Drive', async () => {
  const app = await boot(makeBackend());
  try {
    await until(() => !app.document.getElementById('sideQuota').hidden, 'quota bar hiện');
    assert.match(app.document.getElementById('sideQuotaText').textContent, /\d.*\/.*GB/);
    assert.ok(app.document.getElementById('sideQuota').getAttribute('aria-valuenow') !== null);
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('đa ngôn ngữ VI/EN/ZH: giao diện và hộp thoại Cài đặt của cầu nối đổi theo', async () => {
  const app = await boot(makeBackend());
  try {
    await until(() => rows(app).length === 5, 'Drive đã tải');
    const pick = async (lang) => { const sel = app.document.getElementById('uiLang'); sel.value = lang; sel.dispatchEvent(new app.window.Event('change', { bubbles: true })); };
    const settingsTitle = async () => {
      app.document.dispatchEvent(new app.window.KeyboardEvent('keydown', { key: ',', ctrlKey: true, bubbles: true }));
      const ov = await until(() => app.document.querySelector('.overlay.pd-settings'), 'Cài đặt');
      const title = ov.querySelector('h3').textContent;
      ov.remove();
      return title;
    };
    assert.equal(await settingsTitle(), 'Cài đặt peaDrive');
    await pick('en');
    assert.equal(app.document.documentElement.lang, 'en');
    await until(() => /Buy me a coffee/.test(app.document.getElementById('btnCoffee').textContent), 'trang đã dịch sang EN');
    assert.equal(await settingsTitle(), 'peaDrive Settings');
    await pick('zh');
    await until(() => /请我喝杯咖啡/.test(app.document.getElementById('btnCoffee').textContent), 'trang đã dịch sang ZH');
    assert.equal(await settingsTitle(), 'peaDrive 设置');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('hộp thoại Thông tin ứng dụng: hiện phiên bản và kiểm tra cập nhật', async () => {
  const b = makeBackend();
  const app = await boot(b);
  try {
    await until(() => /9\.9\.9/.test(app.document.getElementById('sideVersion').textContent), 'phiên bản');
    const before = callsTo(b, '/api/update/check').length;
    app.document.getElementById('sideVersion').click();
    const ov = await until(() => app.document.querySelector('.overlay'), 'About');
    assert.match(ov.textContent, /peaDrive/);
    ov.querySelector('#aboutUpd').click();
    await until(() => callsTo(b, '/api/update/check').length > before, 'kiểm tra cập nhật');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

// ───────────────────────── 1. Đa tài khoản: chuyển tức thì ─────────────────────────
test('menu tài khoản lấy danh sách từ backend; chuyển tài khoản KHÔNG đăng nhập lại', async () => {
  const b = makeBackend({ accounts: [ME, OTHER] });
  const app = await boot(b);
  try {
    await until(() => app.document.getElementById('accEmail').textContent.trim() === 'me@example.com', 'đã kết nối');
    app.document.getElementById('accountBtn').click();
    const dd = await until(() => app.document.querySelector('.acc-dd'), 'dropdown tài khoản');
    assert.equal(dd.querySelectorAll('.acc-row').length, 2, 'đủ 2 tài khoản của backend');
    assert.ok(dd.querySelector('.acc-row.on[data-id="me@example.com"]'), 'tài khoản đang dùng được đánh dấu');

    b.calls.length = 0;
    dd.querySelector('.acc-row[data-id="other@example.com"]').click();
    await until(() => hasCall(b, 'POST', '/api/accounts/switch?id=other%40example.com'), 'switch');
    await until(() => app.document.getElementById('accEmail').textContent.trim() === 'other@example.com', 'giao diện đổi sang tài khoản mới');
    await until(() => hasCall(b, 'GET', '/api/drive/list?location=my&folder_id=root'), 'nạp lại Drive của tài khoản mới');
    assert.ok(!app.document.querySelector('.overlay'), 'không có hộp thoại xác nhận');
    assert.ok(!hasCall(b, 'POST', '/api/login') && !hasCall(b, 'POST', '/api/disconnect'), 'không đăng nhập lại / đăng xuất');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('chuyển sang tài khoản đã hết phiên → mở đăng nhập lại; thêm tài khoản → tự chuyển sang tài khoản mới', async () => {
  const b = makeBackend({ accounts: [ME, OTHER], relogin: true });
  const app = await boot(b);
  try {
    await until(() => app.document.getElementById('accEmail').textContent.trim() === 'me@example.com', 'đã kết nối');
    app.document.getElementById('accountBtn').click();
    (await until(() => app.document.querySelector('.acc-row[data-id="other@example.com"]'), 'hàng tài khoản')).click();
    await until(() => hasCall(b, 'POST', '/api/login'), 'đăng nhập lại khi phiên hết hạn');
    // Backend (giả) thêm new@example.com sau ~0,5s; UI phải chờ tới khi signins tăng rồi mới chuyển.
    await until(() => app.document.getElementById('accEmail').textContent.trim() === 'new@example.com', 'đã chuyển sang tài khoản vừa thêm', 9000);
    assert.equal(b.st.accounts.length, 3);
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('đăng xuất: gỡ đúng tài khoản đang dùng, tự chuyển sang tài khoản còn lại', async () => {
  const b = makeBackend({ accounts: [ME, OTHER] });
  const app = await boot(b);
  try {
    await until(() => app.document.getElementById('accEmail').textContent.trim() === 'me@example.com', 'đã kết nối');
    app.document.getElementById('accountBtn').click();
    (await until(() => app.document.querySelector('.acc-dd [data-act="out"]'), 'nút đăng xuất')).click();
    const ov = await until(() => app.document.querySelector('.overlay'), 'xác nhận đăng xuất');
    assert.match(ov.textContent, /me@example\.com/);
    ov.querySelector('.acts button:last-child').click();
    await until(() => hasCall(b, 'POST', '/api/accounts/remove?id=me%40example.com'), 'remove');
    await until(() => app.document.getElementById('accEmail').textContent.trim() === 'other@example.com', 'chuyển sang tài khoản còn lại');
    assert.ok(!hasCall(b, 'POST', '/api/disconnect'));
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

// ───────────────────────── 2. Link Mode ─────────────────────────
const LINK = 'https://drive.google.com/drive/folders/LINKFOLDER_1234567890?resourcekey=0-KEYabc';
async function openLink(app, link = LINK) {
  app.document.getElementById('modeLink').checked = true;
  app.document.getElementById('modeLink').dispatchEvent(new app.window.Event('change', { bubbles: true }));
  app.document.getElementById('driveLinkInput').value = link;
  app.document.getElementById('driveLinkGo').click();
}

test('Link Mode: dán link + truy cập → cả khung Drive chuyển sang nội dung của link, không popup', async () => {
  const b = makeBackend();
  const app = await boot(b);
  try {
    await until(() => rows(app).length === 5, 'Drive của tôi đã tải');
    b.calls.length = 0;
    await openLink(app);
    await until(() => hasCall(b, 'GET', `/api/drive/link?link=${encodeURIComponent(LINK)}`), 'phân giải link');
    await until(() => hasCall(b, 'GET', '/api/drive/list?location=link&folder_id=LINKFOLDER_1234567890&rk=0-KEYabc'), 'liệt kê nội dung link kèm resourcekey');
    await until(() => /Thư mục chia sẻ/.test(app.document.getElementById('driveCrumbs').textContent), 'breadcrumb gốc = tên link');
    assert.ok(!/Drive của tôi/.test(app.document.getElementById('driveCrumbs').textContent), 'không còn dính My Drive');
    assert.equal(app.document.querySelectorAll('.side-btn[data-loc].active').length, 0, 'không mục thanh bên nào đang chọn');
    assert.equal(app.document.getElementById('btnMkdir').disabled, true, 'link chỉ xem/tải: không tạo thư mục');
    assert.ok(!app.document.querySelector('.overlay'), 'KHÔNG có popup nhập link / chọn thư mục đích');
    assert.ok(!hasCall(b, 'POST', '/api/transfer/link'), 'không còn tải thẳng qua link');

    // Tải một mục trong link: resourcekey được chuyển tiếp.
    const item = { ...fx('drive_list').items[1], resource_key: '0-FILEKEY' };
    await app.window.downloadSelected([item]);
    const dl = b.calls.find((c) => c.method === 'POST' && c.url === '/api/transfer/download');
    assert.equal(dl.body.items[0].resource_key, '0-FILEKEY');
    assert.equal(dl.body.dest_dir, fx('local_list').current_path);
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('Link Mode: vào thư mục con theo resourcekey của nó; quay về Drive của tôi thì khôi phục trạng thái cũ', async () => {
  const b = makeBackend();
  const app = await boot(b);
  try {
    await until(() => rows(app).length === 5, 'Drive của tôi đã tải');
    // Đang ở "Có gắn dấu sao" trước khi vào Link Mode → phải quay lại đúng chỗ đó.
    app.document.querySelector('.side-btn[data-loc="starred"]').click();
    await until(() => hasCall(b, 'GET', '/api/drive/list?location=starred&folder_id=root'), 'starred');
    await openLink(app);
    await until(() => /Thư mục chia sẻ/.test(app.document.getElementById('driveCrumbs').textContent), 'đang ở Link Mode');
    await until(() => rows(app).length === 5, 'nội dung link');

    b.calls.length = 0;
    const sub = fx('drive_list').items[0]; // thư mục "Tài liệu"
    app.window.openItem('drive', { ...sub, resource_key: '0-SUBKEY' });
    await until(() => hasCall(b, 'GET', `/api/drive/list?location=link&folder_id=${sub.id}&rk=0-SUBKEY`), 'vào thư mục con với resourcekey riêng');
    assert.match(app.document.getElementById('driveCrumbs').textContent, /Thư mục chia sẻ.*Tài liệu/s);

    b.calls.length = 0;
    app.document.getElementById('modeMy').checked = true;
    app.document.getElementById('modeMy').dispatchEvent(new app.window.Event('change', { bubbles: true }));
    await until(() => hasCall(b, 'GET', '/api/drive/list?location=starred&folder_id=root'), 'quay về vị trí trước đó');
    await until(() => /Có gắn dấu sao/.test(app.document.getElementById('driveCrumbs').textContent), 'breadcrumb về lại Có gắn dấu sao');
    assert.ok(app.document.querySelector('.side-btn[data-loc="starred"].active'));
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('Link Mode: link sai/không truy cập được → báo lỗi và giữ nguyên chế độ hiện tại', async () => {
  const b = makeBackend({ linkError: 'Drive API 404 (notFound): File not found' });
  const app = await boot(b);
  try {
    await until(() => rows(app).length === 5, 'Drive của tôi đã tải');
    b.calls.length = 0;
    await openLink(app);
    await until(() => /404/.test(app.document.getElementById('toastText').textContent), 'toast lỗi');
    assert.ok(!callsTo(b, '/api/drive/list').length, 'không liệt kê gì khi link lỗi');
    assert.match(app.document.getElementById('driveCrumbs').textContent, /Drive của tôi/);
    // Link sai định dạng bị chặn ngay ở giao diện
    b.calls.length = 0;
    app.document.getElementById('driveLinkInput').value = 'khong-phai-link';
    app.document.getElementById('driveLinkGo').click();
    await sleep(100);
    assert.equal(callsTo(b, '/api/drive/link').length, 0);
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('Link Mode: link tới MỘT file chỉ hiện file đó; chọn mục thanh bên thì thoát Link Mode', async () => {
  const b = makeBackend({ linkRoot: LINK_FILE });
  const app = await boot(b);
  try {
    await until(() => rows(app).length === 5, 'Drive của tôi đã tải');
    b.calls.length = 0;
    await openLink(app, 'https://drive.google.com/file/d/LINKFILE_1234567890ab/view');
    await until(() => rows(app).length === 1, 'chỉ một dòng');
    assert.match(rows(app)[0].textContent, /bao-cao\.pdf/);
    assert.equal(callsTo(b, '/api/drive/list').length, 0, 'không liệt kê vì link là file');

    app.document.querySelector('.side-btn[data-loc="shared"]').click();
    await until(() => hasCall(b, 'GET', '/api/drive/list?location=shared&folder_id=root'), 'thanh bên thoát Link Mode');
    assert.ok(!/bao-cao\.pdf/.test(app.document.getElementById('driveList').textContent));
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

// ───────────────────────── 3. Chặn khi phiên bản quá cũ ─────────────────────────
function assertBlocked(app) {
  const ov = app.document.getElementById('pdBlock');
  assert.ok(ov, 'phải có lớp chặn');
  assert.equal(ov.querySelector('p').textContent, OUTDATED);
  assert.equal(ov.querySelector('button').textContent, 'Cập nhật ngay');
  assert.ok([...app.document.body.children].every((el) => el.inert === true), 'toàn bộ app bên dưới bị vô hiệu');
  return ov;
}

test('phiên bản quá cũ lúc khởi động: overlay + modal không thể tắt, nút mở đúng trang cập nhật', async () => {
  const b = makeBackend({ force: true });
  const app = await boot(b);
  try {
    await until(() => app.document.getElementById('pdBlock'), 'lớp chặn');
    const ov = assertBlocked(app);
    // Không tắt được: Esc, Tab, click nền, nhấn phím ngoài modal.
    for (const key of ['Escape', 'Tab']) {
      const ev = new app.window.KeyboardEvent('keydown', { key, bubbles: true, cancelable: true });
      ov.dispatchEvent(ev);
      assert.equal(ev.defaultPrevented, true, `${key} bị chặn`);
    }
    const outside = new app.window.KeyboardEvent('keydown', { key: 'F5', bubbles: true, cancelable: true });
    app.document.body.dispatchEvent(outside);
    assert.equal(outside.defaultPrevented, true, 'phím tắt của app bị chặn');
    ov.dispatchEvent(new app.window.MouseEvent('click', { bubbles: true }));
    assert.ok(app.document.getElementById('pdBlock')?.isConnected, 'click nền không đóng được');
    // Có cố xoá khỏi DOM cũng bị dựng lại.
    app.document.getElementById('pdBlock').remove();
    await until(() => app.document.getElementById('pdBlock')?.isConnected, 'lớp chặn tự dựng lại');

    app.document.querySelector('#pdBlock button').click();
    await until(() => hasCall(b, 'POST', `/api/update/open?url=${encodeURIComponent(UPDATE_URL)}`), 'mở trang cập nhật bằng trình duyệt mặc định');
    assert.ok(app.document.getElementById('pdBlock')?.isConnected, 'bấm cập nhật xong modal vẫn còn');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('lớp chặn dựng khi không có sẵn địa chỉ: nút Cập nhật ngay hỏi backend rồi mở liên kết tải thẳng', async () => {
  const b = makeBackend();
  const app = await boot(b);
  try {
    await until(() => rows(app).length === 5, 'Drive đã tải');
    b.st.force = true; // backend bắt đầu từ chối mọi route (E_OUTDATED): bridge dựng lớp chặn mà chưa biết địa chỉ cập nhật
    await until(() => app.document.getElementById('pdBlock'), 'overlay khi gặp E_OUTDATED');
    app.document.querySelector('#pdBlock button').click();
    // Địa chỉ chỉ khai báo ở backend (config.rs) và lấy qua /api/status — bridge không giữ bản sao nào.
    await until(() => hasCall(b, 'POST', `/api/update/open?url=${encodeURIComponent(UPDATE_URL)}`), 'mở liên kết tải thẳng do backend cung cấp');
    assert.match(UPDATE_URL, /\/app-api\/download\.php\?app=peadrive&os=win$/, 'liên kết tải thẳng, không phải trang thông tin chung');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('backend chặn route (E_OUTDATED) → overlay hiện, UI im lặng (không toast lỗi dồn dập)', async () => {
  const b = makeBackend();
  const app = await boot(b);
  try {
    await until(() => rows(app).length === 5, 'Drive đã tải');
    assert.ok(!app.document.getElementById('pdBlock'));
    b.st.force = true; // máy chủ vừa đổi min_required_version; backend bắt đầu từ chối mọi route
    await until(() => app.document.getElementById('pdBlock'), 'overlay khi gặp E_OUTDATED');
    assertBlocked(app);
    await sleep(900);
    assert.ok(!/đã cũ/.test(app.document.getElementById('toastText').textContent), 'không hiện toast lỗi phiên bản');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('phiên bản còn dùng được: không chặn (kể cả khi có bản mới không bắt buộc)', async () => {
  const b = makeBackend({ updateInfo: { available: true, force: false, version: '1.0.9', current: 'v0.0.1', status: 'update_available', url: UPDATE_URL, notes: '' } });
  const app = await boot(b);
  try {
    await until(() => rows(app).length === 5, 'Drive đã tải');
    await sleep(300);
    assert.ok(!app.document.getElementById('pdBlock'), 'không chặn khi chỉ có bản mới');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('overlay chặn theo ngôn ngữ đang chọn (ZH)', async () => {
  const b = makeBackend({ force: true });
  const app = await boot(b, (w) => w.localStorage.setItem('peadrive.lang', 'zh'));
  try {
    await until(() => app.document.getElementById('pdBlock'), 'lớp chặn');
    assert.equal(app.document.querySelector('#pdBlock p').textContent, '您的应用版本已过旧，请更新到最新版本后继续使用。');
    assert.equal(app.document.querySelector('#pdBlock button').textContent, '立即更新');
  } finally { app.close(); }
});

// ───────────────────────── chọn mục · nhập danh sách · Smart Upload ─────────────────────────
const localRows = (app) => app.document.querySelectorAll('#localList .row');
const localSel = (app) => [...app.document.querySelectorAll('#localList .row.sel .name span')].map((e) => e.textContent);

test('nút Đảo chiều: chưa chọn gì thì chọn tất cả, đã chọn thì đảo ngược vùng chọn', async () => {
  const app = await boot(makeBackend({ local: LOCAL_LIST }));
  try {
    await until(() => localRows(app).length === 5, '5 dòng bên máy');
    const btn = app.document.getElementById('selToggleLocal');
    assert.equal(btn.disabled, false);
    btn.click();                                       // chưa chọn gì → chọn tất cả
    assert.equal(localSel(app).length, 5);
    localRows(app)[2].click();                         // click thường: vùng chọn chỉ còn DSC00123.ARW
    assert.deepEqual(localSel(app), ['DSC00123.ARW']);
    btn.click();                                       // đã có mục chọn → đảo chiều
    assert.equal(localSel(app).length, 4);
    assert.ok(!localSel(app).includes('DSC00123.ARW'));
    btn.click();                                       // đảo lần nữa → trở lại như cũ
    assert.deepEqual(localSel(app), ['DSC00123.ARW']);
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('Nhập danh sách (.txt): chọn đúng các mục trùng tên, bỏ qua dòng chú thích và dòng trống', async () => {
  const app = await boot(makeBackend({ local: LOCAL_LIST }));
  try {
    await until(() => localRows(app).length === 5, '5 dòng bên máy');
    const text = '# ghi chú\r\n\r\nDSC00123\r\n"D:\\Render\\FINAL.MP4"\r\nkhong-co.jpg\r\n';
    const file = { name: 'list.txt', arrayBuffer: async () => Uint8Array.from(Buffer.from(text, 'utf8')).buffer };
    await app.window.importList('local', file);
    // DSC00123 (không đuôi) khớp mọi file cùng tên gốc; đường dẫn Windows và hoa/thường vẫn khớp.
    assert.deepEqual(localSel(app).sort(), ['DSC00123.ARW', 'DSC00123.JPG', 'final.mp4']);
    assert.match(app.document.getElementById('toastText').textContent, /Đã chọn 3 mục/);
    assert.match(app.document.getElementById('toastText').textContent, /1 tên chưa tìm thấy/);
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('Smart Upload: nút chỉ bật khi đang chọn thư mục; hộp thoại gửi đúng nguồn, đích và các đuôi file', async () => {
  const b = makeBackend({ local: LOCAL_LIST });
  const app = await boot(b);
  try {
    await until(() => localRows(app).length === 5 && rows(app).length === 5, '5 dòng bên máy và Drive đã kết nối');
    const btn = app.document.getElementById('btnSmartUpload');
    assert.equal(btn.disabled, true, 'chưa chọn gì → tắt');
    localRows(app)[2].click();                         // chỉ một file
    assert.equal(btn.disabled, true, 'chỉ chọn file → vẫn tắt');
    localRows(app)[0].click();                         // thư mục Clips
    assert.equal(btn.disabled, false, 'chọn thư mục → bật');
    btn.click();
    const dlg = await until(() => app.document.querySelector('.smart-dlg'), 'hộp thoại Smart Upload');
    assert.equal([...dlg.querySelectorAll('.sm-opt input:checked')].map((c) => c.value).join(' '), 'mp4 mov mxf avi arw cr2 cr3 nef dng jpg,jpeg png', 'Video và Ảnh tick sẵn');
    assert.match(dlg.textContent, /ít nhất 2 phút/, 'giải thích luật 2 phút');
    assert.match(dlg.querySelector('.src').textContent, /Drive của tôi › Clips/, 'đích Drive = <thư mục đang mở> › <tên thư mục nguồn>');
    const custom = dlg.querySelector('#smartCustom');
    custom.value = 'braw, .R3D';
    custom.dispatchEvent(new app.window.Event('input', { bubbles: true }));
    dlg.querySelector('[data-a="go"]').click();
    const call = await until(() => b.calls.find((c) => c.method === 'POST' && c.url === '/api/transfer/smart-upload'), 'POST smart-upload');
    assert.equal(call.body.folder_id, 'root');
    // `call.body` do bridge.js (chạy TRONG cửa sổ jsdom) tạo ra nên mảng của nó thuộc "realm" khác; deepEqual ở chế độ strict
    // so cả prototype → luôn lệch dù nội dung giống hệt. Chép sang mảng của Node (Array.from) trước khi so.
    assert.deepEqual(Array.from(call.body.paths), ['/media/render/Clips']);
    assert.deepEqual(Array.from(call.body.extensions), ['mp4', 'mov', 'mxf', 'avi', 'arw', 'cr2', 'cr3', 'nef', 'dng', 'jpg', 'jpeg', 'png', 'braw', 'r3d']);
    await until(() => !app.document.getElementById('qSmart').hidden, 'nút ⚡ hiện sau khi backend theo dõi thư mục');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});

test('quản lý Smart Upload: nút ⚡ hiện khi có thư mục đang theo dõi; danh sách và nút Dừng gọi đúng API', async () => {
  const b = makeBackend({ watchers: [WATCHER] });
  const app = await boot(b);
  try {
    const chip = app.document.getElementById('qSmart');
    await until(() => !chip.hidden, 'nút ⚡ hiện');
    assert.equal(app.document.getElementById('qSmartN').textContent, '1');
    chip.click();
    const item = await until(() => app.document.querySelector('.sm-item'), 'danh sách Smart Upload');
    assert.match(item.textContent, /Đã tải lên 2 · Đã có trên Drive 5 · Chờ ổn định 1/);
    item.querySelector('[data-stop]').click();
    const ok = await until(() => app.document.querySelector('.overlay .danger-btn'), 'hộp thoại xác nhận Dừng');
    ok.click();
    await until(() => hasCall(b, 'POST', '/api/transfer/smart-upload/stop?id=7'), 'POST smart-upload/stop');
    await until(() => app.document.querySelector('.sm-empty'), 'danh sách trống sau khi dừng');
    await until(() => chip.hidden, 'nút ⚡ ẩn lại khi không còn thư mục nào');
    assert.deepEqual(app.errors, []);
  } finally { app.close(); }
});


let failed = 0;
for (const [name, fn] of tests) {
  try { await fn(); console.log(`  ✓ ${name}`); }
  catch (e) { failed++; console.log(`  ✗ ${name}\n      ${e.stack?.split('\n').slice(0, 5).join('\n      ') || e}`); }
}
console.log(failed ? `\n${failed}/${tests.length} bài thử THẤT BẠI` : `\n${tests.length}/${tests.length} bài thử giao diện đều qua`);
process.exit(failed ? 1 : 0);
