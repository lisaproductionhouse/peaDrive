const $ = id => document.getElementById(id);
const icon = (name, cls = '') => `<svg class="i ${cls}"><use href="#i-${name}"/></svg>`;
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));

const S = {
  connected: false,
  ownedOnly: false,   // lọc "Chỉ mục tôi sở hữu" của khung Drive: giữ khi đổi thư mục/vị trí, về tắt khi mở lại app
  mode: 'my',            // 'my' = Drive thông thường; 'link' = Link Mode (khung Drive hiển thị nội dung của một link)
  myState: null,         // cất trạng thái Drive của tôi khi vào Link Mode để quay lại
  accounts: [], accountId: '', signins: 0, loginError: '',   // tài khoản Google do backend giữ (xem /api/status)
  location: 'my',
  local: { path: '', parent: '', items: [], sel: new Set(), anchor: null, free: 0, search: '', sort: null },
  drive: { crumbs: [], items: [], sel: new Set(), anchor: null, search: '', loading: false, error: '', sort: null, linkRoot: null },
  queueVisible: false,
  paused: false,
};

async function api(path, opts = {}) {
  if (opts.json !== undefined) {
    opts.method = opts.method || 'POST';
    opts.headers = { 'Content-Type': 'application/json' };
    opts.body = JSON.stringify(opts.json);
  }
  const res = await fetch(path, opts);
  if (!res.ok) throw new Error((await res.text()).trim() || ('Lỗi ' + res.status));
  return res.json();
}

function formatBytes(b) {
  if (!b || b <= 0) return '0 B';
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  const i = Math.min(Math.floor(Math.log(b) / Math.log(1024)), u.length - 1);
  return (b / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1).replace('.', ',') + ' ' + u[i];
}
/* Tốc độ dạng HTML có khung cố định: số căn phải + đơn vị căn trái + chữ số đều nhau (xem .spd trong style.css). Nhờ vậy khi tốc độ đổi
   số chữ số (3,0 → 24,5 MB/s) chữ không xê dịch và không đẩy phần tử bên cạnh. textContent vẫn là "24,5 MB/s". */
function speedHTML(b) {
  if (!(b > 0)) return '—';
  const [n, u] = formatBytes(b).split(' ');
  return `<span class="spd"><span class="spd-n">${n}</span> <span class="spd-u">${u}/s</span></span>`;
}
function formatEta(sec) {
  if (!isFinite(sec) || sec <= 0) return '—';
  if (sec < 60) return Math.ceil(sec) + ' giây';
  if (sec < 3600) return Math.floor(sec / 60) + ' phút ' + Math.round(sec % 60) + ' giây';
  return Math.floor(sec / 3600) + ' giờ ' + Math.round((sec % 3600) / 60) + ' phút';
}

function fileIcon(item) {
  if (item.kind === 'drive') return icon('hdd', 'fi-drive');
  if (item.is_dir) return icon('folder', 'fi-folder');
  if (item.is_google_doc) return icon('file', 'fi-gdoc');
  const ext = (item.name.split('.').pop() || '').toLowerCase();
  if (/^(jpe?g|png|gif|webp|bmp|heic|heif|tiff?|jfif|raw|cr2|cr3|nef|arw|dng|psd|svg)$/.test(ext)) return icon('img', 'fi-img');
  if (/^(mp4|mov|avi|mkv|wmv|m4v|webm|mts|mxf|flv)$/.test(ext)) return icon('vid', 'fi-vid');
  if (/^(zip|rar|7z|tar|gz|bz2|xz|iso)$/.test(ext)) return icon('zip', 'fi-zip');
  if (ext === 'pdf') return icon('file', 'fi-pdf');
  if (/^(docx?|xlsx?|pptx?|txt|csv|rtf|odt|ods)$/.test(ext)) return icon('file', 'fi-doc');
  return icon('file', 'fi-file');
}

let toastTimer;
function toast(msg, isErr = false) {
  $('toastText').textContent = msg;
  $('toast').classList.toggle('err', isErr);
  $('toast').querySelector('use').setAttribute('href', isErr ? '#i-warn' : '#i-check');
  $('toast').classList.remove('hide');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => $('toast').classList.add('hide'), isErr ? 5000 : 2600);
}

/* Chính sách quyền riêng tư (Google yêu cầu có liên kết ngay trong giao diện app). Link ngoài đánh dấu bằng data-ext được mở
   bằng trình duyệt mặc định qua cầu nối (window.open → /api/open), không điều hướng chính cửa sổ app. */
const PRIVACY_URL = 'https://peamark.com/privacy';
document.addEventListener('click', e => {
  const a = e.target.closest && e.target.closest('a[data-ext]');
  if (!a) return;
  e.preventDefault();
  window.open(a.href, '_blank');
});

function dialog({ title, message = '', input = null, ok = 'Đồng ý', danger = false, cancel = 'Huỷ' }) {
  return new Promise(resolve => {
    const ov = document.createElement('div');
    ov.className = 'overlay';
    ov.innerHTML = `<div class="dialog"><h3>${esc(title)}</h3>${message ? `<p>${esc(message)}</p>` : ''}
      ${input !== null ? `<input value="${esc(input)}">` : ''}
      <div class="acts">${cancel ? `<button class="ghost">${esc(cancel)}</button>` : ''}<button class="${danger ? 'danger-btn' : 'primary'}">${esc(ok)}</button></div></div>`;
    document.body.appendChild(ov);
    const field = ov.querySelector('input');
    const done = v => { ov.remove(); resolve(v); };
    ov.querySelector('.acts button:last-child').onclick = () => done(field ? field.value.trim() : true);
    const g = ov.querySelector('.ghost'); if (g) g.onclick = () => done(null);
    ov.onclick = e => { if (e.target === ov) done(null); };
    ov.onkeydown = e => { if (e.key === 'Escape') done(null); if (e.key === 'Enter') done(field ? field.value.trim() : true); };
    setTimeout(() => { if (field) { field.focus(); const dot = field.value.lastIndexOf('.'); field.setSelectionRange(0, dot > 0 ? dot : field.value.length); } else ov.querySelector('.acts button:last-child').focus(); });
  });
}
const alertBox = (title, message) => dialog({ title, message, ok: 'Đã hiểu', cancel: null });

/* ---------------- Danh sách có chọn nhiều ---------------- */
/* ---------------- Sắp xếp (giống menu Sắp xếp của Google Drive) ---------------- */
const SORT_KEYS = { name: 'Tên', modified: 'Ngày sửa đổi', size: 'Dung lượng', kind: 'Loại file' };
function directionLabel(key, asc) {
  if (key === 'modified') return asc ? 'Cũ đến mới' : 'Mới đến cũ';
  if (key === 'size') return asc ? 'Nhỏ đến lớn' : 'Lớn đến nhỏ';
  return asc ? 'A → Z' : 'Z → A';
}
function loadSort(side) {
  try { const v = JSON.parse(localStorage.getItem('sort-' + side)); if (v && SORT_KEYS[v.key]) return v; } catch (e) {}
  return { key: 'name', asc: true, foldersFirst: true };
}
function setSort(side, patch) {
  S[side].sort = Object.assign({}, S[side].sort, patch);
  try { localStorage.setItem('sort-' + side, JSON.stringify(S[side].sort)); } catch (e) {}
  renderList(side);
}
const collator = new Intl.Collator('vi', { numeric: true, sensitivity: 'base' });
const itemDate = (side, i) => Date.parse(side === 'local' ? i.mod_time : i.modified_time) || 0;
const itemKind = i => i.is_dir || i.kind ? '' : (i.name.includes('.') ? i.name.split('.').pop().toLowerCase() : '');
function sortItems(side, list) {
  const o = S[side].sort;
  // Ổ đĩa và lối tắt trong màn "Máy tính" giữ nguyên thứ tự của hệ thống.
  if (side === 'local' && S.local.path === '::drives') return list;
  return list.slice().sort((a, b) => {
    const fa = !!a.is_dir, fb = !!b.is_dir;
    if (o.foldersFirst && fa !== fb) return fa ? -1 : 1;
    let c = 0;
    if (o.key === 'modified') c = itemDate(side, a) - itemDate(side, b);
    else if (o.key === 'size') c = (a.size || 0) - (b.size || 0);
    else if (o.key === 'kind') c = collator.compare(itemKind(a), itemKind(b));
    else c = collator.compare(a.name, b.name);
    if (c === 0) return collator.compare(a.name, b.name); // bằng nhau thì theo tên, thứ tự luôn ổn định
    return o.asc ? c : -c;
  });
}
function paintSortControls(side) {
  const o = S[side].sort;
  $(side === 'local' ? 'sortLocal' : 'sortDrive').innerHTML = `${icon('sort')}${SORT_KEYS[o.key]}`;
  if (side === 'drive') {   // đang lọc "Chỉ mục tôi sở hữu": nút mở menu có chấm báo nhẹ + tooltip
    $('sortDrive').classList.toggle('filtered', S.ownedOnly);
    $('sortDrive').title = S.ownedOnly ? 'Sắp xếp · đang lọc: chỉ mục tôi sở hữu' : 'Sắp xếp';
  }
  document.querySelectorAll(`.list-head[data-side="${side}"] [data-sort]`).forEach(h => {
    const on = h.dataset.sort === o.key;
    h.classList.toggle('on', on);
    h.textContent = h.textContent.replace(/ [↑↓]$/, '') + (on ? (o.asc ? ' ↑' : ' ↓') : '');
  });
}
function sortMenu(side, x, y) {
  const o = S[side].sort;
  openMenu(x, y, [
    { heading: 'Sắp xếp theo' },
    ...Object.keys(SORT_KEYS).map(k => ({ label: SORT_KEYS[k], checked: o.key === k, run: () => setSort(side, { key: k }) })),
    '-', { heading: 'Chiều sắp xếp' },
    { label: directionLabel(o.key, true), checked: o.asc, run: () => setSort(side, { asc: true }) },
    { label: directionLabel(o.key, false), checked: !o.asc, run: () => setSort(side, { asc: false }) },
    '-', { heading: 'Thư mục' },
    { label: 'Trên cùng', checked: o.foldersFirst, run: () => setSort(side, { foldersFirst: true }) },
    { label: 'Xếp lẫn với file', checked: !o.foldersFirst, run: () => setSort(side, { foldersFirst: false }) },
    ...(side === 'drive' ? ['-', { heading: 'Hiển thị' }, { label: 'Chỉ mục tôi sở hữu', checked: S.ownedOnly, run: toggleOwnedOnly }] : []),
  ]);
}
// Bật/tắt lọc "Chỉ mục tôi sở hữu" (khung Drive). Mục đang bị ẩn thì bỏ khỏi vùng chọn để không thao tác nhầm.
function toggleOwnedOnly() {
  S.ownedOnly = !S.ownedOnly;
  const shown = new Set(visible('drive').map(i => i.id));
  S.drive.sel = new Set([...S.drive.sel].filter(k => shown.has(k)));
  paintSortControls('drive');
  renderList('drive');
}
// Chú thích cho dấu "không phải của bạn" ở icon: chủ sở hữu (hàng nằm ngoài vùng tự dịch nên dịch tại đây).
function ownerTip(item) {
  const s = item.owner ? `Chủ sở hữu: ${item.owner}` : 'Không phải của bạn';
  return LANG === 'vi' ? s : (tr(s) ?? s);
}
function formatDate(t) {
  const d = new Date(t);
  if (!t || isNaN(d)) return '';
  const p = n => String(n).padStart(2, '0');
  return `${p(d.getDate())}/${p(d.getMonth() + 1)}/${String(d.getFullYear()).slice(2)} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

// Đọc sau khi SORT_KEYS đã khai báo: đọc sớm hơn sẽ rơi vào lỗi TDZ và lặng lẽ về mặc định.
S.local.sort = loadSort('local'); S.drive.sort = loadSort('drive');
paintSortControls('local'); paintSortControls('drive');   // hiện sẵn nút sắp xếp và cột "Tên ↑" ở cả 2 khung ngay từ đầu

function visible(side) {
  const st = S[side];
  const q = st.search.trim().toLowerCase();
  let items = st.items;
  if (side === 'drive' && S.ownedOnly) items = items.filter(i => i.owned_by_me === true);   // chưa biết chủ sở hữu thì không tính là của tôi
  return sortItems(side, q ? items.filter(i => i.name.toLowerCase().includes(q)) : items);
}
const keyOf = (side, item) => side === 'local' ? item.path : item.id;
const selectedItems = side => visible(side).filter(i => S[side].sel.has(keyOf(side, i)));

function handleRowClick(side, item, e) {
  const st = S[side];
  const key = keyOf(side, item);
  const list = visible(side);
  if (e.shiftKey && st.anchor !== null) {
    const a = list.findIndex(i => keyOf(side, i) === st.anchor);
    const b = list.findIndex(i => keyOf(side, i) === key);
    if (!e.ctrlKey) st.sel.clear();
    if (a >= 0 && b >= 0) list.slice(Math.min(a, b), Math.max(a, b) + 1).forEach(i => st.sel.add(keyOf(side, i)));
  } else if (e.ctrlKey || e.metaKey) {
    st.sel.has(key) ? st.sel.delete(key) : st.sel.add(key);
    st.anchor = key;
  } else {
    st.sel.clear(); st.sel.add(key); st.anchor = key;
  }
  paintSelection(side);
}

function renderList(side) {
  const st = S[side];
  const el = $(side === 'local' ? 'localList' : 'driveList');
  const list = visible(side);
  el.innerHTML = '';
  const frag = document.createDocumentFragment();
  list.forEach(item => {
    const key = keyOf(side, item);
    const row = document.createElement('div');
    row.className = 'row' + (st.sel.has(key) ? ' sel' : '') + (item.is_google_doc ? ' dim' : '');
    row.dataset.key = key;
    row.draggable = !(side === 'local' && item.kind);
    let size = item.is_dir ? '—' : formatBytes(item.size);
    if (item.kind === 'drive') size = item.total ? `${formatBytes(item.free)} trống` : '—';
    if (item.kind === 'place') size = '';
    if (item.is_google_doc) size = 'Google Docs';
    const ownTip = side === 'drive' && item.owned_by_me === false ? ownerTip(item) : '';   // chỉ mục KHÔNG phải của mình mới có chấm
    row.innerHTML = `<span class="ficon"${ownTip ? ` title="${esc(ownTip)}"` : ''}>${fileIcon(item)}${ownTip ? '<i class="own-dot" aria-hidden="true"></i>' : ''}</span>
      <span class="name"><span title="${esc(item.name)}">${esc(item.name)}</span>${item.is_dir ? `<button class="open" title="Mở thư mục (hoặc nhấp đúp)">${icon('chev-r')}</button>` : ''}</span>
      <span class="date">${item.kind ? '' : formatDate(side === 'local' ? item.mod_time : item.modified_time)}</span>
      <span class="size">${size}</span>`;
    row.onclick = e => { handleRowClick(side, item, e); el.focus({ preventScroll: true }); };
    row.ondblclick = () => openItem(side, item);
    row.oncontextmenu = e => {
      e.preventDefault();
      if (!st.sel.has(key)) { st.sel.clear(); st.sel.add(key); st.anchor = key; paintSelection(side); }
      showMenu(side, e.clientX, e.clientY);
    };
    const open = row.querySelector('.open');
    if (open) open.onclick = e => { e.stopPropagation(); openItem(side, item); };
    row.ondragstart = e => {
      if (!st.sel.has(key)) { st.sel.clear(); st.sel.add(key); st.anchor = key; paintSelection(side); }
      e.dataTransfer.setData('application/x-peadrive', side);
      e.dataTransfer.effectAllowed = 'copy';
    };
    frag.appendChild(row);
  });
  el.appendChild(frag);
  renderEmpty(side, el, list);
  paintSortControls(side);
  updateFooter(side);
  updateToolbar();
}

function paintSelection(side) {
  const st = S[side];
  document.querySelectorAll(`#${side === 'local' ? 'localList' : 'driveList'} .row`).forEach(r => r.classList.toggle('sel', st.sel.has(r.dataset.key)));
  updateFooter(side);
  updateToolbar();
}

/* ---------------- Chọn tất cả / Đảo chiều vùng chọn ----------------
   Nút ở cột đầu tiêu đề danh sách: chưa chọn mục nào → chọn tất cả; đã có mục được chọn → đảo ngược vùng chọn. */
function toggleSelectAll(side) {
  const st = S[side], list = visible(side).filter(i => !i.kind);
  if (!list.length) return;
  const any = list.some(i => st.sel.has(keyOf(side, i)));
  list.forEach(i => {
    const k = keyOf(side, i);
    if (!any || !st.sel.has(k)) st.sel.add(k); else st.sel.delete(k);
  });
  st.anchor = null;
  paintSelection(side);
}
function paintSelToggles() {
  ['local', 'drive'].forEach(side => {
    const b = $(side === 'local' ? 'selToggleLocal' : 'selToggleDrive');
    const list = visible(side).filter(i => !i.kind), any = list.some(i => S[side].sel.has(keyOf(side, i)));
    const t = any ? 'Đảo chiều vùng chọn' : 'Chọn tất cả';
    b.disabled = !list.length; b.classList.toggle('on', any);
    if (b.title !== t) { b.title = t; b.setAttribute('aria-label', t); }
  });
}

function renderEmpty(side, el, list) {
  let html = '';
  if (side === 'drive' && !S.connected) {
    html = `<div class="empty"><div class="ring"><div>${icon('cloud')}</div></div><h4>Kết nối không gian của bạn</h4>
      <p>Đăng nhập Google Drive để duyệt file<br>và tải lên chỉ bằng một lần kéo thả.</p>
      <button class="primary" onclick="login()">${icon('plus')}Kết nối Google Drive</button><p style="font-size:9px">Đăng nhập an toàn qua Google</p>
        <div class="priv-note" role="note"><p>peaDrive xử lý file Google Drive ngay trên máy bạn và không gửi dữ liệu Drive tới máy chủ nào của peaDrive. Việc peaDrive sử dụng, và chuyển giao cho bất kỳ ứng dụng nào khác, thông tin nhận từ Google API sẽ tuân thủ Chính sách Dữ liệu Người dùng của Dịch vụ API Google (Google API Services User Data Policy), kể cả các yêu cầu Sử dụng Hạn chế (Limited Use).</p>
        <a class="priv-link" href="${PRIVACY_URL}" data-ext>Chính sách quyền riêng tư</a></div></div>`;
  } else if (side === 'drive' && S.drive.error) {
    html = `<div class="empty"><div class="ring"><div>${icon('warn')}</div></div><h4>Chưa tải được thư mục</h4><p>${esc(S.drive.error)}</p>
      <button class="primary" onclick="loadDrive()">Thử lại</button></div>`;
  } else if (side === 'drive' && S.drive.loading && !list.length) {
    html = `<div class="empty"><p>Đang tải…</p></div>`;
  } else if (!list.length) {
    const searching = S[side].search.trim();
    html = `<div class="empty"><div class="ring"><div>${icon(side === 'drive' ? 'up' : 'folder')}</div></div>
      <p>${searching ? 'Không tìm thấy mục phù hợp' : side === 'drive' && S.ownedOnly && S.drive.items.length ? 'Không có mục nào do bạn sở hữu trong thư mục này' : side === 'drive' ? 'Thả file vào đây để tải lên' : 'Không có mục nào'}</p></div>`;
  }
  if (html) el.insertAdjacentHTML('beforeend', html);
}

function updateFooter(side) {
  const st = S[side];
  const list = visible(side);
  const sel = selectedItems(side);
  const bytes = sel.reduce((n, i) => n + (i.is_dir ? 0 : (i.size || 0)), 0);
  const left = sel.length ? `${sel.length} mục đã chọn: ${formatBytes(bytes)}` : `${list.length} mục`;
  if (side === 'local') {
    $('localFootL').textContent = left;
    $('localFootR').textContent = S.local.path === '::drives' || !S.local.free ? '' : `${formatBytes(S.local.free)} trống`;
  } else {
    $('driveFootL').textContent = S.connected ? left : '—';
    $('driveFootR').textContent = S.ownedOnly ? 'Đang lọc: chỉ mục tôi sở hữu' : '';
  }
}

function updateToolbar() {
  const inFolder = S.local.path && S.local.path !== '::drives';
  const driveWritable = S.connected && S.mode !== 'link' && !(S.location !== 'my' && S.drive.crumbs.length === 0);
  const dl = selectedItems('drive').filter(i => !i.is_google_doc && i.can_download !== false);
  const ul = selectedItems('local').filter(i => !i.kind);
  $('btnDownload').disabled = !S.connected || !dl.length || !inFolder;
  $('btnUpload').disabled = !driveWritable || !ul.length;
  $('btnMkdir').disabled = !driveWritable;
  $('btnLink').disabled = !S.connected;
  $('btnDownload').title = !inFolder ? 'Mở một thư mục trên máy để làm nơi lưu' : `Tải ${dl.length} mục về ${S.local.path}`;
  $('btnUpload').title = ul.length ? `Tải ${ul.length} mục lên thư mục Drive đang mở` : 'Chọn file hoặc thư mục bên máy để tải lên';
  // Smart Upload: chỉ bấm được khi đang chọn ít nhất một thư mục ở khung "Máy của tôi"
  const smartDirs = ul.filter(i => i.is_dir);
  $('btnSmartUpload').disabled = !smartDirs.length;
  $('btnSmartUpload').title = smartDirs.length ? 'Quét thư mục đã chọn và chỉ tải lên các định dạng bạn chọn' : 'Chọn một thư mục bên khung Máy của tôi để dùng Smart Upload';
  paintSelToggles();
}

/* ---------------- Máy tính ---------------- */
let localReq = 0;
async function loadLocal(path = S.local.path) {
  const my = ++localReq;
  try {
    const d = await api('/api/local/list?path=' + encodeURIComponent(path || ''));
    if (my !== localReq) return; // đã có yêu cầu mới hơn (ví dụ tự làm mới khi cửa sổ được focus)
    const changed = d.current_path !== S.local.path;
    S.local.path = d.current_path; S.local.parent = d.parent_path; S.local.items = d.items || []; S.local.free = d.free || 0;
    if (changed) { S.local.sel.clear(); S.local.anchor = null; S.local.search = ''; $('localSearch').value = ''; $('localList').scrollTop = 0; }
    else { const keys = new Set(S.local.items.map(i => i.path)); [...S.local.sel].forEach(k => keys.has(k) || S.local.sel.delete(k)); }
    renderLocalCrumbs();
    renderList('local');
  } catch (e) { if (my === localReq) toast(e.message, true); }
}

function renderLocalCrumbs() {
  const el = $('localCrumbs');
  const p = S.local.path;
  // Nút Back nằm NGOÀI vùng cuộn `.trail`: đường dẫn dài chỉ cuộn phần breadcrumb, không đẩy/che nút Back.
  let html = `<button class="back" title="Lên thư mục cha" ${p === '::drives' ? 'disabled' : ''}>${icon('chev-l')}</button><div class="trail">`;
  html += `<button class="crumb${p === '::drives' ? ' cur' : ''}" data-path="::drives">${icon('pc')}<span class="nm">Máy tính</span></button>`;
  if (p !== '::drives') {
    const parts = p.split(/[\\/]/).filter(Boolean);
    let acc = '';
    parts.forEach((part, i) => {
      acc = i === 0 ? (part.endsWith(':') ? part + '\\' : '/' + part) : acc.replace(/[\\/]$/, '') + '\\' + part;
      html += `<span class="crumb-sep">${icon('chev-r')}</span><button class="crumb${i === parts.length - 1 ? ' cur' : ''}" data-path="${esc(acc)}" title="${esc(part)}"><span class="nm">${esc(part)}</span></button>`;
    });
  }
  el.innerHTML = html + '</div>';
  el.querySelector('.back').onclick = () => S.local.parent && loadLocal(S.local.parent);
  el.querySelectorAll('.crumb').forEach(c => c.onclick = () => loadLocal(c.dataset.path));
  const trail = el.querySelector('.trail');
  trail.scrollLeft = trail.scrollWidth; // luôn thấy thư mục hiện tại ở cuối
}

/* ---------------- Google Drive ---------------- */
const LOCATIONS = { link: 'Link Drive', my: 'Drive của tôi', shared: 'Được chia sẻ', team: 'Bộ nhớ dùng chung', starred: 'Có gắn dấu sao', trash: 'Thùng rác', storage: 'Bộ nhớ' };
const driveFolderID = () => S.drive.crumbs.length ? S.drive.crumbs[S.drive.crumbs.length - 1].id : (S.mode === 'link' && S.drive.linkRoot ? S.drive.linkRoot.id : 'root');
// Link cũ cần kèm resourcekey của thư mục đang mở (backend gửi qua header X-Goog-Drive-Resource-Keys)
const driveRk = () => {
  if (S.mode !== 'link') return '';
  const c = S.drive.crumbs, k = c.length ? c[c.length - 1].rk : (S.drive.linkRoot && S.drive.linkRoot.resource_key);
  return k ? '&rk=' + encodeURIComponent(k) : '';
};

let driveReq = 0;
async function loadDrive(keepSelection = false) {
  if (!S.connected) { renderDriveCrumbs(); renderList('drive'); return; }
  const my = ++driveReq;
  S.drive.loading = true; S.drive.error = '';
  if (!keepSelection) { S.drive.sel.clear(); S.drive.anchor = null; }
  renderDriveCrumbs();
  if (!keepSelection) { S.drive.items = []; renderList('drive'); }
  let revealed = null;
  try {
    let items;
    if (S.mode === 'link' && S.drive.linkRoot && !S.drive.linkRoot.is_dir) items = [S.drive.linkRoot];   // link tới MỘT file: chỉ hiện file đó
    else items = (await api(`/api/drive/list?location=${S.location}&folder_id=${encodeURIComponent(driveFolderID())}${driveRk()}`)).items || [];
    if (my !== driveReq) return;
    S.drive.items = items;
    const keys = new Set(S.drive.items.map(i => i.id)); [...S.drive.sel].forEach(k => keys.has(k) || S.drive.sel.delete(k));
    const pick = S.drive.reveal; S.drive.reveal = null;   // mở từ link tệp: chọn đúng tệp đó
    if (pick && keys.has(pick)) { S.drive.sel = new Set([pick]); S.drive.anchor = pick; revealed = pick; }
  } catch (e) {
    if (my !== driveReq) return;
    S.drive.error = e.message; S.drive.items = [];
    if (/Chưa kết nối|hết hạn|đăng nhập lại/i.test(e.message)) refreshStatus();
  }
  S.drive.loading = false;
  renderList('drive');
  if (revealed) [...document.querySelectorAll('#driveList .row')].find(r => r.dataset.key === revealed)?.scrollIntoView({ block: 'center' });
}

function renderDriveCrumbs() {
  const el = $('driveCrumbs');
  const c = S.drive.crumbs;
  const linkRoot = S.mode === 'link' ? S.drive.linkRoot : null;
  const locIcon = linkRoot ? 'link' : { my: 'hdd', shared: 'people', team: 'building', starred: 'star', trash: 'trash', storage: 'storage' }[S.location];
  let html = `<button class="back" title="Quay lại thư mục trước" ${c.length ? '' : 'disabled'}>${icon('chev-l')}</button><div class="trail">`;
  html += `<button class="crumb${c.length ? '' : ' cur'}" data-i="-1">${icon(locIcon)}<span class="nm">${linkRoot ? esc(linkRoot.name) : LOCATIONS[S.location]}</span></button>`;
  c.forEach((cr, i) => html += `<span class="crumb-sep">${icon('chev-r')}</span><button class="crumb${i === c.length - 1 ? ' cur' : ''}" data-i="${i}" title="${esc(cr.name)}"><span class="nm">${esc(cr.name)}</span></button>`);
  el.innerHTML = html + '</div>';
  el.querySelector('.back').onclick = () => { if (c.length) { c.pop(); S.drive.search = ''; $('driveSearch').value = ''; loadDrive(); } };
  el.querySelectorAll('.crumb').forEach(b => b.onclick = () => { S.drive.crumbs = c.slice(0, +b.dataset.i + 1); S.drive.search = ''; $('driveSearch').value = ''; loadDrive(); });
  const trail = el.querySelector('.trail');
  trail.scrollLeft = trail.scrollWidth; // luôn thấy thư mục hiện tại ở cuối
  document.querySelectorAll('.side-btn[data-loc]').forEach(b => b.classList.toggle('active', S.connected && S.mode !== 'link' && b.dataset.loc === S.location));
}

function openItem(side, item) {
  if (side === 'local') {
    if (item.is_dir) loadLocal(item.path);
    return;
  }
  if (item.is_dir) {
    S.drive.crumbs.push({ id: item.id, name: item.name, rk: item.resource_key || '' });
    S.drive.search = ''; $('driveSearch').value = '';
    $('driveList').scrollTop = 0;
    loadDrive();
  } else if (item.web_view_link) {
    window.open(item.web_view_link, '_blank');
  }
}

/* ---------------- Tài khoản ---------------- */
async function refreshStatus() {
  try {
    const d = await api('/api/status');
    applyReady(d);
    $('sideVersion').textContent = 'v' + d.version;
    const was = S.connected;
    S.connected = !!d.connected;
    S.accounts = Array.isArray(d.accounts) ? d.accounts : []; S.accountId = d.active || ''; S.signins = d.signins || 0;
    if (d.login_error) S.loginError = d.login_error;
    $('accountBtn').classList.toggle('on', S.connected);
    $('btnDriveRefresh').disabled = !S.connected;
    $('sortDrive').disabled = !S.connected;
    $('driveSearch').disabled = !S.connected;
    if (S.connected) {
      if (!was) { loadDrive(); loadAbout(); }
    } else {
      $('accEmail').textContent = 'Kết nối Google Drive'; $('accountBtn').title = 'Đăng nhập Google Drive'; $('sbQuota').textContent = ''; paintSideQuota(0);
      exitLinkMode(true);
      S.drive.items = []; S.drive.crumbs = [];
      renderDriveCrumbs(); renderList('drive');
    }
  } catch (e) { setReady(false, 'backend'); }
}

function paintSideQuota(usage, limit) {
  const q = $('sideQuota');
  if (!usage) { q.hidden = true; return; }
  const pct = limit ? Math.min(100, usage / limit * 100) : 0;
  q.hidden = false;
  q.querySelector('.bar').hidden = !limit;
  $('sideQuotaFill').style.width = pct + '%';
  q.classList.toggle('warn', pct >= 80 && pct < 95);
  q.classList.toggle('crit', pct >= 95);
  q.setAttribute('aria-valuenow', Math.round(pct));
  $('sideQuotaText').textContent = limit ? `${formatBytes(usage)} / ${formatBytes(limit)}` : `${formatBytes(usage)} đã dùng`;
  q.title = limit ? `${pct.toFixed(1)}% dung lượng Drive đã dùng` : 'Dung lượng Drive đã dùng';
}
async function loadAbout() {
  try {
    const a = await api('/api/drive/about');
    $('accEmail').textContent = a.email || a.name || 'Google Drive';
    S.accountEmail = a.email || '';
    paintSideQuota(a.usage, a.limit);
    $('accountBtn').title = (a.name ? a.name + ' · ' : '') + 'Quản lý kết nối';
    $('sbQuota').textContent = a.usage ? `Drive: ${formatBytes(a.usage)}${a.limit ? ' / ' + formatBytes(a.limit) : ' đã dùng'}` : '';
  } catch (e) {}
}

let loginTimer;
// Mở trang đăng nhập Google (luôn có hộp chọn tài khoản). Xong khi backend báo thêm một lần đăng nhập thành công.
async function login() {
  const before = S.signins || 0, wasConnected = S.connected;
  S.loginError = '';
  try {
    await api('/api/login', { method: 'POST' });
    toast('Đã mở trang đăng nhập Google trong trình duyệt');
    clearInterval(loginTimer);
    let tries = 0;
    loginTimer = setInterval(async () => {
      await refreshStatus();
      if ((S.signins || 0) > before) { clearInterval(loginTimer); toast('Đã kết nối Google Drive'); if (wasConnected) onAccountChanged(); }
      else if (S.loginError || ++tries > 300) clearInterval(loginTimer);
    }, 1000);
  } catch (e) { toast(e.message, true); }
}

/* Dropdown tài khoản: danh sách do backend giữ (S.accounts, lấy từ /api/status). Mỗi tài khoản có token riêng trong keychain
   nên chuyển tài khoản là TỨC THÌ: không mở trình duyệt, không đăng nhập lại. Tác vụ đang chạy vẫn dùng tài khoản đã tạo ra chúng. */
function getAccounts() { return S.accounts || []; }
// Sau khi đổi tài khoản: Drive của tôi của tài khoản mới, thoát Link Mode, nạp lại dung lượng + hàng đợi.
function onAccountChanged() {
  exitLinkMode(true);
  S.location = 'my';
  S.drive = freshDrive(S.drive.sort);
  $('driveSearch').value = '';
  renderDriveCrumbs(); loadDrive(); loadAbout(); lastQueueSig = ''; pollQueue();
}
async function switchAccount(acc) {
  if (!acc) return;
  try {
    const r = await api('/api/accounts/switch?id=' + encodeURIComponent(acc.id), { method: 'POST' });
    if (!r.ok) { toast('Phiên đăng nhập của ' + acc.email + ' đã hết hạn — hãy đăng nhập lại tài khoản này', true); return login(); }
    await refreshStatus();
    toast('Đã chuyển sang ' + acc.email);
    onAccountChanged();
  } catch (e) { toast(e.message, true); }
}
async function signOut() {
  const cur = getAccounts().find(a => a.id === S.accountId), who = cur ? cur.email : 'Google Drive';
  if (!await dialog({ title: 'Đăng xuất ' + who + '?', message: 'peaDrive sẽ xoá phiên đăng nhập của tài khoản này trên máy này. Các tài khoản khác vẫn giữ nguyên; tác vụ đang chạy của tài khoản này sẽ được tạm dừng.', ok: 'Đăng xuất', danger: true })) return;
  try {
    const r = await api('/api/accounts/remove?id=' + encodeURIComponent(S.accountId), { method: 'POST' });
    await refreshStatus();
    if (r.active) { toast('Đã chuyển sang ' + r.active); onAccountChanged(); } else { lastQueueSig = ''; pollQueue(); }
  } catch (e) { toast(e.message, true); }
}
let accDD = null;
function closeAccDD() { if (!accDD) return; accDD.remove(); accDD = null; $('accountBtn').setAttribute('aria-expanded', 'false'); }
function toggleAccDD() {
  if (accDD) return closeAccDD();
  const list = getAccounts().slice(), cur = S.connected ? S.accountId : '';
  if (!S.connected && !list.length) return login();
  closeMenu();
  const row = a => `<button class="acc-row${a.id === cur ? ' on' : ''}" role="menuitem" data-id="${esc(a.id)}"><span class="av">${esc((a.name || a.email)[0] || '?')}</span><span class="who"><b>${esc(a.name || a.email)}</b>${a.name ? `<small>${esc(a.email)}</small>` : ''}</span>${a.id === cur ? icon('tick', 'acc-check') : ''}</button>`;
  const el = accDD = document.createElement('div');
  el.className = 'menu acc-dd'; el.setAttribute('role', 'menu');
  el.innerHTML = `<div class="head">Tài khoản</div>${list.map(row).join('')}<hr>`
    + `<button data-act="add" role="menuitem">${icon('plus')}<span>Thêm tài khoản khác</span></button>`
    + (S.connected ? `<button data-act="web" role="menuitem">${icon('link')}<span>Mở Google Drive trên web</span></button><hr><button class="danger" data-act="out" role="menuitem">${icon('logout')}<span>Đăng xuất</span></button>` : '');
  el.onclick = e => {
    const b = e.target.closest('button'); if (!b) return;
    closeAccDD();
    if (b.dataset.id) { if (b.dataset.id !== cur) switchAccount(list.find(a => a.id === b.dataset.id)); }
    else if (b.dataset.act === 'add') login();
    else if (b.dataset.act === 'web') window.open('https://drive.google.com/', '_blank');
    else if (b.dataset.act === 'out') signOut();
  };
  document.body.appendChild(el);
  const r = $('accountBtn').getBoundingClientRect(), w = el.getBoundingClientRect().width;
  el.style.left = Math.max(8, Math.min(r.left, innerWidth - w - 8)) + 'px';
  el.style.top = (r.bottom + 6) + 'px';
  $('accountBtn').setAttribute('aria-expanded', 'true');
  el.querySelector('button').focus();
}
document.addEventListener('mousedown', e => { if (accDD && !accDD.contains(e.target) && !$('accountBtn').contains(e.target)) closeAccDD(); });
document.addEventListener('keydown', e => { if (e.key === 'Escape' && accDD) { closeAccDD(); $('accountBtn').focus(); } });
addEventListener('resize', closeAccDD);

/* Bảng thông tin ứng dụng */
function aboutDialog() {
  if (document.querySelector('.overlay')) return;
  const ver = $('sideVersion').textContent.trim();
  const ov = document.createElement('div');
  ov.className = 'overlay';
  ov.innerHTML = `<div class="dialog about-dlg" role="dialog" aria-modal="true" aria-label="Thông tin ứng dụng">
    <button class="about-x" title="Đóng" aria-label="Đóng">${icon('x')}</button>
    <h3>peaDrive${/\d/.test(ver) ? ' ' + esc(ver) : ''}</h3>
    <dl>
      <dt>Nhà phát triển</dt><dd>peaMark</dd>
      <dt>Website</dt><dd><a href="https://peamark.com" target="_blank" rel="noopener noreferrer">peamark.com</a></dd>
      <dt>Quyền riêng tư</dt><dd><a href="${PRIVACY_URL}" data-ext>peamark.com/privacy</a></dd>
      <dt>Liên hệ</dt><dd><a href="mailto:PeaMarkCloud@gmail.com">PeaMarkCloud@gmail.com</a></dd>
    </dl>
    <div class="acts"><button class="ghost" id="aboutUpd">Kiểm tra cập nhật</button></div>
  </div>`;
  const close = () => { document.removeEventListener('keydown', onKey, true); ov.remove(); };
  const onKey = e => { if (e.key === 'Escape') close(); };
  document.addEventListener('keydown', onKey, true);
  ov.addEventListener('mousedown', e => { if (e.target === ov) close(); });
  ov.querySelector('.about-x').onclick = close;
  ov.querySelector('#aboutUpd').onclick = async e => {
    const b = e.currentTarget, old = b.textContent;
    b.disabled = true; b.textContent = 'Đang kiểm tra…';
    try {
      const d = await api('/api/update/check');
      if (d.available) { UPD.info = d; UPD.later = false; paintUpdate(); close(); toast('Đã có peaDrive ' + d.version); }
      else toast('Bạn đang dùng bản mới nhất');
    } catch (err) { toast(err.message || 'Không kiểm tra được bản cập nhật', true); }
    b.disabled = false; b.textContent = old;
  };
  document.body.appendChild(ov);
}

/* ---------------- Truyền file ---------------- */
async function downloadSelected(items = selectedItems('drive')) {
  items = items.filter(i => !i.is_google_doc && i.can_download !== false);
  if (!items.length) return;
  if (!S.local.path || S.local.path === '::drives') return alertBox('Chưa chọn nơi lưu', 'Hãy mở một thư mục bên khung "Máy của bạn" (ví dụ ổ D: hoặc Downloads) rồi tải về lại.');
  try {
    await api('/api/transfer/download', { json: { dest_dir: S.local.path, items: items.map(i => ({ id: i.id, name: i.name, size: i.size, is_dir: i.is_dir, resource_key: i.resource_key || '' })) } });
    toast(`Đã thêm ${items.length} mục vào hàng đợi tải xuống`);
    showQueue(true);
  } catch (e) { toast(e.message, true); }
}

/* Tải lên thủ công: gửi file/thư mục đang chọn bên "Máy của tôi" cho backend; quét thư mục, dựng cây thư mục đích và đối chiếu file
   đã có (không tải trùng) chạy ở backend. Body: { folder_id: <thư mục Drive đang mở>, paths: [...], use_fallback?: true }.
   Quyền drive.file chỉ cho ghi vào "Drive của tôi" và thư mục do peaDrive tạo: nếu thư mục đang mở không ghi được, backend trả
   { needs_fallback: true, blocked_name, destination } mà CHƯA làm gì; ta hỏi người dùng rồi gọi lại với use_fallback = true. */
function uploadSummary(r) {
  if (!r.queued && r.skipped) return `Tất cả ${r.skipped} file đã có sẵn trên Drive`;
  if (!r.queued) return `Đã tạo ${r.folders} thư mục trên Drive`;
  return r.skipped ? `Đã thêm ${r.queued} file vào hàng đợi tải lên · ${r.skipped} file đã có trên Drive` : `Đã thêm ${r.queued} file vào hàng đợi tải lên`;
}
async function uploadSelected(items = selectedItems('local')) {
  items = items.filter(i => !i.kind);
  if (!items.length) return;
  if (S.mode === 'link') return alertBox('Không thể tải lên ở chế độ Link', 'Chế độ Link chỉ để xem và tải xuống. Hãy chuyển về Drive của tôi để tải lên.');
  if (S.location !== 'my' && S.drive.crumbs.length === 0) return alertBox('Chưa chọn thư mục Drive', 'Hãy mở một thư mục trên Google Drive làm nơi tải lên.');
  if (S.uploading) return;               // đang chuẩn bị một lượt khác: tránh bấm đúp
  S.uploading = true;
  try {
    if (items.some(i => i.is_dir)) toast('Đang chuẩn bị danh sách tải lên…');
    const body = { folder_id: driveFolderID(), paths: items.map(i => i.path) };
    let r = await api('/api/transfer/upload', { json: body });
    if (r.needs_fallback) {
      const ok = await dialog({
        title: 'Thư mục này không do peaDrive tạo',
        message: `peaDrive chỉ ghi được vào “Drive của tôi” và các thư mục do chính nó tạo (quyền drive.file), nên không tải lên thẳng vào “${r.blocked_name}” được. Tải lên “${r.destination}” thay thế? Sau đó bạn có thể kéo các mục sang đúng chỗ trên Google Drive.`,
        ok: 'Tải lên thư mục thay thế',
      });
      if (!ok) return;
      r = await api('/api/transfer/upload', { json: { ...body, use_fallback: true } });
    }
    if (r.warnings && r.warnings.length) toast(r.warnings.join(' · '), true);
    else toast(uploadSummary(r));
    showQueue(true);
    pollQueue();
  } catch (e) { toast(e.message, true); }
  finally { S.uploading = false; }
}

/* ---------------- Nhập danh sách (.txt) ----------------
   Mỗi mục là một tên file/thư mục (có thể kèm đường dẫn). Các mục cách nhau bằng xuống dòng, dấu phẩy (,) hoặc dấu chấm phẩy (;), dùng riêng hay trộn lẫn đều được.
   Dòng trống và dòng bắt đầu bằng # bị bỏ qua.
   Khớp không phân biệt hoa/thường. Mục không có đuôi (vd: DSC00123) khớp mọi file cùng tên gốc (DSC00123.ARW, DSC00123.JPG).
   Ngoại lệ: nếu cả dòng trùng đúng tên một mục trong khung và tên đó có chứa , hoặc ; (vd: "Báo cáo, tháng 5.docx") thì giữ nguyên là một mục, không tách.
   Các mục khớp được chọn (thay cho vùng chọn hiện tại); chỉ đối chiếu với những mục đang hiển thị trong khung. */
async function importList(side, file) {
  if (!file) return;
  const all = visible(side), items = all.filter(i => !i.kind);
  if (!items.length) return toast('Khung này chưa có mục nào để đối chiếu', true);
  let text;
  try {
    const b = new Uint8Array(await file.arrayBuffer());
    text = new TextDecoder(b[0] === 0xFF && b[1] === 0xFE ? 'utf-16le' : b[0] === 0xFE && b[1] === 0xFF ? 'utf-16be' : 'utf-8').decode(b);
  } catch (e) { return toast('Không đọc được file danh sách', true); }
  const norm = s => s.normalize('NFC').trim().toLowerCase();
  const byName = new Map(), byStem = new Map();
  const put = (m, k, it) => m.has(k) ? m.get(k).push(it) : m.set(k, [it]);
  items.forEach(it => {
    const n = norm(it.name), dot = n.lastIndexOf('.');
    put(byName, n, it);
    if (dot > 0 && !it.is_dir) put(byStem, n.slice(0, dot), it);
  });
  // Tên cuối của một mục: bỏ ngoặc kép bao quanh, dấu / \ ở cuối và phần đường dẫn phía trước
  const base = s => norm(s.trim().replace(/^["']+|["']+$/g, '').replace(/[\\/]+$/, '').split(/[\\/]/).pop());
  const SEP = /[,;]/;
  const names = new Set();
  text.split(/\r?\n/).map(l => l.trim()).filter(l => l && !l.startsWith('#')).forEach(l => {
    const whole = base(l);
    if (SEP.test(whole) && (byName.has(whole) || byStem.has(whole))) names.add(whole);   // tên file vốn có , hoặc ; → giữ nguyên, không tách
    else l.split(SEP).forEach(p => { const n = base(p); if (n) names.add(n); });
  });
  if (!names.size) return toast('File danh sách trống', true);
  const hit = new Set(); let missing = 0;
  names.forEach(n => {
    const found = byName.get(n) || byStem.get(n);
    if (found) found.forEach(it => hit.add(keyOf(side, it))); else missing++;
  });
  const st = S[side];
  st.sel.clear(); hit.forEach(k => st.sel.add(k));
  st.anchor = hit.size ? [...hit][0] : null;
  paintSelection(side);
  const idx = all.findIndex(i => hit.has(keyOf(side, i)));
  const row = idx >= 0 && document.querySelector(`#${side === 'local' ? 'localList' : 'driveList'} .row:nth-child(${idx + 1})`);
  if (row) row.scrollIntoView({ block: 'nearest' });
  if (!hit.size) toast('Không có mục nào khớp với danh sách', true);
  else toast(missing ? `Đã chọn ${hit.size} mục · còn ${missing} tên chưa tìm thấy` : `Đã chọn ${hit.size} mục khớp với danh sách`);
}

/* ---------------- Smart Upload ----------------
   Giao diện chỉ gom lựa chọn rồi gửi cho backend; quét thư mục, chờ render và xác thực file chạy nền ở backend.
   Body gửi đi: { folder_id: <thư mục Drive đang mở>, paths: [<thư mục đang chọn bên máy>], extensions: ['mp4', 'mov', ...] }
   (extensions: chữ thường, không dấu chấm, đã gộp checkbox + ô nhập tay). */
const SMART_UPLOAD_API = '/api/transfer/smart-upload';
const SMART_GROUPS = [   // [nhãn hiển thị, các đuôi file cách nhau bằng dấu phẩy]. Thêm/bớt định dạng chỉ cần sửa bảng này
  { label: 'Video', opts: [['MP4', 'mp4'], ['MOV', 'mov'], ['MXF', 'mxf'], ['AVI', 'avi']] },
  { label: 'Ảnh', opts: [['ARW', 'arw'], ['CR2', 'cr2'], ['CR3', 'cr3'], ['NEF', 'nef'], ['DNG', 'dng'], ['JPG/JPEG', 'jpg,jpeg'], ['PNG', 'png']] },
];
function smartUploadDialog() {
  if (document.querySelector('.overlay')) return;
  const dirs = selectedItems('local').filter(i => i.is_dir && !i.kind);
  if (!dirs.length) return;
  if (!S.connected) return alertBox('Chưa kết nối Google Drive', 'Hãy kết nối Google Drive trước khi dùng Smart Upload.');
  if (S.mode === 'link') return alertBox('Không thể tải lên ở chế độ Link', 'Chế độ Link chỉ để xem và tải xuống. Hãy chuyển về Drive của tôi để tải lên.');
  if (S.location !== 'my' && !S.drive.crumbs.length) return alertBox('Chưa chọn thư mục Drive', 'Hãy mở một thư mục trên Google Drive làm nơi tải lên.');
  const dest = S.drive.crumbs.length ? S.drive.crumbs[S.drive.crumbs.length - 1].name : LOCATIONS[S.location];
  const ov = document.createElement('div');
  ov.className = 'overlay';
  ov.innerHTML = `<div class="dialog link-dlg smart-dlg" role="dialog" aria-modal="true" aria-label="Smart Upload">
    <div class="head"><div class="badge-ic">${icon('bolt')}</div><div><h3>Smart Upload</h3><p>Quét thư mục đã chọn và chỉ tải lên các định dạng bạn chọn</p></div></div>
    <div class="src"><span>Thư mục nguồn</span><b class="name" title="${esc(dirs.map(d => d.path).join('\n'))}">${esc(dirs[0].name + (dirs.length > 1 ? ` (+${dirs.length - 1})` : ''))}</b><span>Đích Drive</span><b class="name">${esc(dest + ' › ' + dirs[0].name + (dirs.length > 1 ? ` (+${dirs.length - 1})` : ''))}</b></div>
    ${SMART_GROUPS.map(g => `<div class="sm-group"><label class="sm-head"><input type="checkbox" class="sm-all" checked><span>${g.label}</span></label>
      <div class="sm-opts">${g.opts.map(([l, e]) => `<label class="sm-opt"><input type="checkbox" value="${e}" checked><span>${l}</span></label>`).join('')}</div></div>`).join('')}
    <div class="lbl">Định dạng khác</div>
    <input id="smartCustom" placeholder="vd: braw, r3d, wav" autocomplete="off" spellcheck="false">
    <p class="hint">Nhập đuôi file, cách nhau bằng dấu phẩy. Không cần dấu chấm.</p>
    <p class="hint">File chỉ được tải lên khi lần sửa đổi cuối cách hiện tại ít nhất 2 phút, để không tải file đang render dở.</p>
    <div class="acts"><button class="ghost" data-a="cancel">Huỷ</button><button class="primary" data-a="go">Bắt đầu tải lên</button></div></div>`;
  document.body.appendChild(ov);
  const go = ov.querySelector('[data-a="go"]'), custom = ov.querySelector('#smartCustom');
  const close = () => ov.remove();
  const customExts = () => custom.value.toLowerCase().split(/[\s,;]+/).map(s => s.replace(/^\*?\./, '')).filter(s => /^[a-z0-9]{1,10}$/.test(s));
  const collect = () => [...new Set([...ov.querySelectorAll('.sm-opt input:checked')].flatMap(c => c.value.split(',')).concat(customExts()))];
  const refresh = () => {   // đồng bộ checkbox của nhóm (chọn hết / một phần) và khoá nút khi chưa có định dạng nào
    ov.querySelectorAll('.sm-group').forEach(g => {
      const cs = [...g.querySelectorAll('.sm-opt input')], on = cs.filter(c => c.checked).length, head = g.querySelector('.sm-all');
      head.checked = on === cs.length; head.indeterminate = on > 0 && on < cs.length;
    });
    go.disabled = !collect().length;
  };
  ov.addEventListener('change', e => {
    if (e.target.classList.contains('sm-all')) e.target.closest('.sm-group').querySelectorAll('.sm-opt input').forEach(c => c.checked = e.target.checked);
    refresh();
  });
  custom.addEventListener('input', refresh);
  ov.querySelector('[data-a="cancel"]').onclick = close;
  ov.addEventListener('mousedown', e => { if (e.target === ov) close(); });
  ov.onkeydown = e => {
    if (e.key === 'Escape') close();
    else if (e.key === 'Enter' && e.target === custom && !e.isComposing) { e.preventDefault(); go.click(); }
  };
  go.onclick = async () => {
    const extensions = collect();
    if (!extensions.length) return;
    go.disabled = true;
    try {
      const r = await api(SMART_UPLOAD_API, { json: { folder_id: driveFolderID(), paths: dirs.map(d => d.path), extensions } });
      close();
      if (r && r.warnings && r.warnings.length) toast(r.warnings.join(' · '), true);
      else toast(`Đã gửi Smart Upload cho ${dirs.length} thư mục`);
      showQueue(true);
      pollQueue();
    } catch (e) { go.disabled = false; toast(e.message, true); }
  };
  refresh();
  setTimeout(() => go.focus());
}

/* Quản lý Smart Upload: danh sách thư mục backend đang theo dõi (nút ⚡ trong thanh Hàng đợi, chỉ hiện khi có ít nhất một thư mục).
   GET  /api/transfer/smart-upload            → { items: [{ id, source, destination, extensions, uploaded, skipped, queued, waiting, error? }] }
   POST /api/transfer/smart-upload/stop?id=…  → dừng một thư mục (bỏ `id` để dừng tất cả) */
async function smartManageDialog() {
  if (document.querySelector('.overlay')) return;
  const ov = document.createElement('div');
  ov.className = 'overlay';
  ov.innerHTML = `<div class="dialog link-dlg smart-dlg" role="dialog" aria-modal="true" aria-label="Smart Upload">
    <div class="head"><div class="badge-ic">${icon('bolt')}</div><div><h3>Smart Upload đang chạy</h3><p>Tự tải lên file mới sau khi file không bị sửa đổi trong 2 phút (render xong)</p></div></div>
    <div class="sm-list"></div>
    <div class="acts"><button class="ghost" data-a="close">Đóng</button></div></div>`;
  document.body.appendChild(ov);
  const list = ov.querySelector('.sm-list');
  let timer = 0;
  const close = () => { clearInterval(timer); ov.remove(); };
  const paint = async () => {
    if (!ov.isConnected) return clearInterval(timer);
    let items;
    try { items = (await api(SMART_UPLOAD_API)).items || []; } catch (e) { return; }
    list.innerHTML = items.length ? items.map(w => `<div class="sm-item">
      <div class="sm-item-top"><b class="name" title="${esc(w.source)}">${esc(w.source)}</b><button class="ghost" data-stop="${esc(w.id)}">Dừng</button></div>
      <div class="sm-path name">${esc(w.destination)}</div>
      <div class="sm-stats">Đã tải lên ${w.uploaded} · Đã có trên Drive ${w.skipped} · Chờ ổn định ${w.waiting}</div>
      <div class="sm-exts">${esc((w.extensions || []).join(' '))}</div>
      ${w.error ? `<div class="sm-err name">${esc(w.error)}</div>` : ''}</div>`).join('') : '<p class="sm-empty">Chưa có thư mục nào đang được theo dõi</p>';
  };
  list.onclick = async e => {
    const b = e.target.closest('[data-stop]');
    if (!b) return;
    if (!await dialog({ title: 'Dừng theo dõi thư mục này?', message: 'Các file đã vào hàng đợi vẫn tiếp tục tải lên.', ok: 'Dừng', danger: true, cancel: 'Không' })) return;
    try { await api(`${SMART_UPLOAD_API}/stop?id=${encodeURIComponent(b.dataset.stop)}`, { method: 'POST' }); toast('Đã dừng Smart Upload'); }
    catch (err) { return toast(err.message, true); }
    paint(); pollQueue();
  };
  ov.querySelector('[data-a="close"]').onclick = close;
  ov.addEventListener('mousedown', e => { if (e.target === ov) close(); });
  ov.onkeydown = e => { if (e.key === 'Escape') close(); };
  await paint();
  timer = setInterval(paint, 2000);
  ov.querySelector('[data-a="close"]').focus();
}

function showQueue(v) {
  S.queueVisible = v;
  $('queue').classList.toggle('hidden', !v);
  document.body.classList.toggle('queue-open', v);
  $('sbShowQueue').hidden = v;
  $('sideTransfers').classList.toggle('active', v);
}

const STATUS = { restored: 'Chưa xong từ lần trước', held: 'Để sau', waiting: 'Đang chờ', working: 'Đang truyền', paused: 'Tạm dừng', completed: 'Hoàn tất', skipped: 'Đã có sẵn', failed: 'Lỗi', cancelled: 'Đã huỷ' };
let lastQueueSig = '';
let refreshDriveAfter = false, refreshLocalAfter = false;

/* Chấm trạng thái ở thanh dưới: xanh lá = sẵn sàng, đỏ = chưa sẵn sàng. Không kèm chữ (chữ chỉ nằm ở tooltip và aria-label).
   "Sẵn sàng" = backend trả lời được + đã đăng nhập Google Drive + liên lạc được với Google. Dữ liệu thật đến từ:
   - backend: trường `ready` / `ready_reason` của /api/transfer/queue (hỏi mỗi 0,5 s) và /api/status (mỗi 15 s);
   - gọi backend thất bại → chưa sẵn sàng với lý do 'backend'.
   Lý do (hiện ở tooltip): 'not_signed_in' | 'offline' | 'backend'. */
const READY_WHY = {
  not_signed_in: 'Chưa đăng nhập Google Drive',
  offline: 'Không kết nối được tới Google Drive',
  backend: 'Không liên lạc được với ứng dụng',
};
function setReady(ok, reason) {
  ok = !!ok;
  const key = ok ? 'ready' : 'not-ready|' + (reason || '');
  const d = $('sbDot');
  if (d.dataset.state === key) return;   // không đổi thì bỏ qua, khỏi ghi DOM mỗi lần poll
  d.dataset.state = key;
  d.classList.toggle('ready', ok);
  d.classList.toggle('not-ready', !ok);
  const t = ok ? 'Sẵn sàng' : (READY_WHY[reason] ? `Chưa sẵn sàng: ${READY_WHY[reason]}` : 'Chưa sẵn sàng');
  d.title = t; d.setAttribute('aria-label', t);
}
/* JSON của backend thiếu `ready` (backend cũ) thì coi là sẵn sàng. */
const applyReady = d => setReady(d.ready !== false, d.ready_reason);

async function pollQueue() {
  let d;
  try { d = await api('/api/transfer/queue'); } catch (e) { setReady(false, 'backend'); return; }
  applyReady(d);
  const items = d.items || [];
  S.paused = d.paused;
  $('qPause').textContent = d.paused ? 'Tiếp tục' : 'Tạm dừng tất cả';
  const smart = d.smart || 0;   // số thư mục Smart Upload backend đang theo dõi
  $('qSmart').hidden = !smart;
  $('qSmartN').textContent = smart;

  const active = items.filter(i => ['waiting', 'working', 'paused'].includes(i.status));
  const working = items.filter(i => i.status === 'working');
  $('transferBadge').hidden = !active.length;
  if (UPD.busy !== active.length > 0) { UPD.busy = active.length > 0; paintUpdate(); }
  $('transferBadge').textContent = active.length;

  // Làm mới khung đích khi một đợt truyền xong
  if (working.some(i => i.direction === 'upload')) refreshDriveAfter = true;
  if (working.some(i => i.direction === 'download')) refreshLocalAfter = true;
  if (!active.length) {
    if (refreshDriveAfter) { refreshDriveAfter = false; loadDrive(true); loadAbout(); }
    if (refreshLocalAfter) { refreshLocalAfter = false; loadLocal(); }
  }

  const speed = working.reduce((n, i) => n + (i.speed || 0), 0);
  const total = active.reduce((n, i) => n + i.total_bytes, 0), done = active.reduce((n, i) => n + i.done_bytes, 0);
  if (d.paused && active.length) $('sbText').innerHTML = '<span style="color:var(--yellow)">Tác vụ đang tạm dừng</span>';
  else if (active.length) {
    const pct = total ? Math.floor(done / total * 100) : 0;
    const up = working.length && working.every(i => i.direction === 'upload');
    $('sbText').innerHTML = `${up ? 'Đang tải lên' : 'Đang truyền file'} (${pct}%) · còn ${active.length} file${speed ? `<span class="spd-sep"> · </span>${speedHTML(speed)}` : ''}`;
  } else $('sbText').textContent = '';   // rảnh: không hiện chữ, trạng thái thể hiện bằng chấm màu (xem setReady)
  const finished = items.filter(i => i.status === 'completed' || i.status === 'skipped').length;
  $('qSummary').innerHTML = items.length ? `${finished}/${items.length} xong${speed ? `<span class="spd-sep"> · </span>${speedHTML(speed)}` : ''}` : '';

  const sig = JSON.stringify(items.map(i => [i.id, i.status, i.done_bytes, i.speed, i.destination]));
  if (sig === lastQueueSig) return;
  lastQueueSig = sig;
  const el = $('qList');
  if (!items.length) { el.innerHTML = `<div class="q-empty">Không có tiến trình nào trong hàng đợi. Chọn file rồi bấm Tải lên / Tải xuống, hoặc kéo thả giữa hai khung.</div>`; return; }
  // Đang chạy lên đầu, rồi đang chờ, cuối cùng là đã xong
  const order = { working: 0, paused: 0, waiting: 1, restored: 1, held: 1, failed: 2, cancelled: 3, completed: 3, skipped: 3 };
  const sorted = items.slice().sort((a, b) => order[a.status] - order[b.status]);
  el.innerHTML = sorted.slice(0, 400).map(i => {
    const pct = i.total_bytes ? Math.min(100, i.done_bytes / i.total_bytes * 100) : (i.status === 'completed' ? 100 : 0);
    const up = i.direction === 'upload';
    const eta = i.status === 'working' && i.speed ? formatEta((i.total_bytes - i.done_bytes) / i.speed) : '—';
    const active = ['waiting', 'working', 'paused'].includes(i.status);
    const retry = ['failed', 'restored', 'held'].includes(i.status);
    return `<div class="q-row" title="${esc(i.error_msg || '')}">
      <span class="dir ${up ? 'up' : 'down'}">${icon(up ? 'up' : 'down')}${up ? 'Tải lên' : 'Tải xuống'}</span>
      <span class="ell" title="${esc(i.source)}">${esc(i.name)}</span>
      <span class="ell wide mono" title="${esc(i.destination)}">${esc(i.destination)}</span>
      <span class="ell st-${i.status}">${STATUS[i.status] || i.status}${i.error_msg ? ': ' + esc(i.error_msg) : ''}</span>
      <span class="wide mono">${formatBytes(i.done_bytes)} / ${formatBytes(i.total_bytes)}</span>
      <span class="bar"><div style="width:${pct.toFixed(1)}%"></div></span>
      <span class="mono">${i.status === 'working' ? speedHTML(i.speed) : '—'}</span>
      <span class="wide mono">${eta}</span>
      <span style="display:flex;justify-content:flex-end">${retry ? `<button class="icon-btn" title="${i.status === 'failed' ? 'Thử lại từ chỗ đã dừng' : 'Tiếp tục'}" onclick="retryTransfer('${i.id}')">${icon(i.status === 'failed' ? 'refresh' : 'play')}</button>` : ''}<button class="icon-btn" title="${active || i.status === 'restored' || i.status === 'held' ? 'Huỷ tác vụ' : 'Xoá khỏi danh sách'}" onclick="cancelTransfer('${i.id}')">${icon('x')}</button></span>
    </div>`;
  }).join('') + (sorted.length > 400 ? `<div class="q-empty">… và ${sorted.length - 400} mục khác</div>` : '');
}
async function cancelTransfer(id) { await api('/api/transfer/cancel?id=' + encodeURIComponent(id), { method: 'POST' }); lastQueueSig = ''; pollQueue(); }
async function retryTransfer(id) { await api('/api/transfer/retry?id=' + encodeURIComponent(id), { method: 'POST' }); lastQueueSig = ''; pollQueue(); }

/* ---------------- Tiếp tục phiên làm việc dở dang ---------------- */
async function checkSession() {
  let d;
  try { d = await api('/api/session'); } catch (e) { return; }
  $('qAuto').checked = !!d.auto_resume;
  if (!d.restored || document.querySelector('.overlay')) return;
  showQueue(true);
  const ov = document.createElement('div');
  ov.className = 'overlay';
  ov.innerHTML = `<div class="dialog" style="width:min(520px,calc(100vw - 32px))"><h3>Tiếp tục phiên làm việc trước?</h3>
    <p>Lần trước peaDrive dừng khi còn ${d.restored} tác vụ chưa xong${d.bytes_left > 0 ? ` (còn khoảng ${formatBytes(d.bytes_left)})` : ''}. Phần đã truyền vẫn được giữ, tiếp tục sẽ chạy từ chỗ đã dừng.</p>
    <div class="acts" style="flex-wrap:wrap"><button class="ghost" data-a="discard" style="color:var(--red);margin-right:auto">Bỏ các tác vụ này</button><button class="ghost" data-a="later">Để sau</button><button class="ghost" data-a="always">Luôn tự tiếp tục</button><button class="primary" data-a="resume">Tiếp tục</button></div></div>`;
  document.body.appendChild(ov);
  ov.querySelectorAll('[data-a]').forEach(b => b.onclick = async () => {
    ov.remove();
    let a = b.dataset.a;
    if (a === 'always') { await api('/api/session?action=auto_on', { method: 'POST' }); $('qAuto').checked = true; a = 'resume'; }
    await api('/api/session?action=' + a, { method: 'POST' });
    lastQueueSig = ''; pollQueue();
  });
  ov.onkeydown = e => { if (e.key === 'Escape') ov.querySelector('[data-a="later"]').click(); };
  setTimeout(() => ov.querySelector('.primary').focus());
}
$('qAuto').onchange = e => api('/api/session?action=' + (e.target.checked ? 'auto_on' : 'auto_off'), { method: 'POST' });

/* ---------------- Kéo đổi chiều cao hàng đợi ---------------- */
(function () {
  const q = $('queue'), handle = $('qResize');
  const apply = h => { h = Math.max(90, Math.min(h, innerHeight - 260)); q.style.setProperty('--qh', h + 'px'); return h; };
  try { const saved = +localStorage.getItem('queue-height'); if (saved) apply(saved); } catch (e) {}
  handle.addEventListener('mousedown', e => {
    e.preventDefault();
    const startY = e.clientY, startH = q.getBoundingClientRect().height;
    handle.classList.add('on'); document.body.style.cursor = 'ns-resize';
    const move = ev => apply(startH + (startY - ev.clientY));
    const up = () => {
      handle.classList.remove('on'); document.body.style.cursor = '';
      removeEventListener('mousemove', move); removeEventListener('mouseup', up);
      try { localStorage.setItem('queue-height', Math.round(q.getBoundingClientRect().height)); } catch (e) {}
    };
    addEventListener('mousemove', move); addEventListener('mouseup', up);
  });
})();

/* ---------------- Tải từ link Google Drive ---------------- */
const DRIVE_LINK = /(drive|docs)\.google\.com\/\S+|^[\w-]{25,60}$/;
// Link Mode: dán link rồi bấm truy cập → TOÀN BỘ khung Drive hiển thị nội dung của link (file hoặc thư mục), tách hẳn khỏi
// Drive của tôi. Chọn mục rồi tải bằng nút Tải xuống hoặc kéo sang khung "Máy của bạn". Chọn lại "Drive của tôi" để quay về.
function freshDrive(sort) { return { crumbs: [], items: [], sel: new Set(), anchor: null, search: '', loading: false, error: '', sort, linkRoot: null, reveal: null }; }
function setModeRadios() { $('modeLink').checked = S.mode === 'link'; $('modeMy').checked = S.mode !== 'link'; }

async function enterLinkMode(text) {
  const go = $('driveLinkGo');
  if (go.disabled) return;
  go.disabled = true;
  try {
    const root = await api('/api/drive/link?link=' + encodeURIComponent(text));   // phân giải link; lỗi → ở nguyên chế độ hiện tại
    if (root.my_drive) return openInMyDrive(root.my_drive);   // link nằm trong Drive của tôi (tài khoản đang đăng nhập): mở thẳng vị trí đó
    if (S.mode !== 'link') S.myState = { location: S.location, drive: S.drive };   // cất Drive của tôi để quay lại
    S.mode = 'link'; S.location = 'link';
    S.drive = freshDrive(S.drive.sort); S.drive.linkRoot = root;
    $('driveSearch').value = '';
    setModeRadios();
    loadDrive();
  } catch (e) { toast(e.message, true); }
  finally { go.disabled = false; }
}

function exitLinkMode(silent) {
  if (S.mode !== 'link') return;
  const back = S.myState; S.myState = null;
  S.mode = 'my';
  S.location = back ? back.location : 'my';
  S.drive = back ? back.drive : freshDrive(S.drive.sort);
  $('driveSearch').value = S.drive.search || '';
  setModeRadios();
  if (!silent) loadDrive();
}

/* Link trỏ tới thư mục/tệp nằm trong Drive của tôi của tài khoản đang đăng nhập (backend báo `my_drive`): thay vì Link Mode,
   chuyển sang Drive của tôi và mở thẳng vị trí đó — link thư mục → mở thư mục; link tệp → mở thư mục chứa rồi chọn tệp. */
function openInMyDrive(spot) {
  S.myState = null;   // đang ở Link Mode thì bỏ trạng thái đã cất: ta mở một vị trí mới trong Drive của tôi
  S.mode = 'my'; S.location = 'my';
  S.drive = freshDrive(S.drive.sort);
  S.drive.crumbs = spot.crumbs.map(c => ({ id: c.id, name: c.name, rk: '' }));
  S.drive.reveal = spot.select || null;
  $('driveSearch').value = ''; $('driveLinkInput').value = '';
  setModeRadios();
  toast('Link này nằm trong Drive của tôi — đã mở đúng vị trí');
  loadDrive();
}

// Hiện ô dán link (và điền sẵn nếu có); go = true thì truy cập luôn (thả link vào khung).
function openLinkField(text = '', go = false) {
  if (!S.connected) return alertBox('Chưa kết nối Google Drive', 'Vui lòng kết nối tài khoản Google Drive trước khi mở link.');
  $('modeLink').checked = true;
  const inp = $('driveLinkInput');
  if (text) inp.value = text;
  if (go && text) return submitDriveLink();
  inp.focus(); inp.select();
}

/* ---------------- Gửi 1 ly Cafe ---------------- */
function coffeeDialog() {
  if (document.querySelector('.overlay')) return;
  const ov = document.createElement('div');
  ov.className = 'overlay';
  ov.innerHTML = `<div class="dialog coffee-dlg">
    <div class="cup">${icon('coffee')}</div>
    <h3>Gửi 1 ly cà phê</h3>
    <p>Hãy gửi 1 ly cafe để mình có thêm động lực cải tiến những chức năng tốt hơn</p>
    <div class="qr"><img src="qrtpbank.svg" alt="Mã QR ngân hàng TPBank"></div>
    <p style="margin-bottom:8px">Quét mã QR (VietQR · Napas 247)</p>
    <div class="acct"><span style="color:var(--muted)">TPBank</span><b>01457020001</b><button title="Sao chép số tài khoản">${icon('copy')}</button></div>
    <div class="owner">HOANG QUOC HUNG</div>
    <div class="acts"><button class="ghost">Thôi</button><button class="primary">OK</button></div></div>`;
  document.body.appendChild(ov);
  const close = () => ov.remove();
  ov.querySelector('.acct button').onclick = async () => { await navigator.clipboard.writeText('01457020001').catch(() => {}); toast('Đã sao chép số tài khoản'); };
  ov.querySelectorAll('.acts button').forEach(b => b.onclick = close);
  ov.onclick = e => { if (e.target === ov) close(); };
  ov.onkeydown = e => { if (e.key === 'Escape') close(); };
  ov.tabIndex = -1; ov.focus();
}

/* ---------------- Thông báo bản mới ---------------- */
// Chỉ nhắc khi hàng đợi rảnh; "Bỏ qua bản này" nhớ theo từng phiên bản.
const UPD = { info: null, later: false, busy: false };
function readSkipped() { try { return localStorage.getItem('update-skipped-version'); } catch (e) { return null; } }
async function checkUpdate() {
  try {
    const d = await api('/api/update/check');
    if (d.available && d.version !== readSkipped()) UPD.info = d;
  } catch (e) {}
  paintUpdate();
}
function paintUpdate() {
  const d = UPD.info;
  const show = !!d && !UPD.later && !UPD.busy;
  $('updateBar').hidden = !show;
  if (!d) return;
  $('updTitle').textContent = 'Đã có peaDrive ' + d.version;
  $('updCurrent').textContent = 'Bạn đang dùng ' + d.current;
  $('updNotes').hidden = !d.notes;
}
async function getUpdate() {
  if (!UPD.info) return;
  try { await api('/api/update/open?url=' + encodeURIComponent(UPD.info.url), { method: 'POST' }); UPD.later = true; paintUpdate(); toast('Đang tải bản mới trong trình duyệt'); }
  catch (e) { toast(e.message, true); }
}
$('updGet').onclick = getUpdate;
$('updLater').onclick = () => { UPD.later = true; paintUpdate(); };
$('updSkip').onclick = () => { try { localStorage.setItem('update-skipped-version', UPD.info.version); } catch (e) {} UPD.info = null; paintUpdate(); };
$('updNotes').onclick = async () => {
  if (await dialog({ title: `peaDrive ${UPD.info.version} có gì mới`, message: UPD.info.notes, ok: 'Tải về', cancel: 'Để sau' })) getUpdate();
};

/* ---------------- Menu chuột phải ---------------- */
let menuEl = null;
function closeMenu() { if (menuEl) { menuEl.remove(); menuEl = null; } }
function openMenu(x, y, entries) {
  closeMenu();
  menuEl = document.createElement('div');
  menuEl.className = 'menu';
  entries.forEach(en => {
    if (en === '-') { menuEl.appendChild(document.createElement('hr')); return; }
    if (en.heading) { const h = document.createElement('div'); h.className = 'head'; h.textContent = en.heading; menuEl.appendChild(h); return; }
    const b = document.createElement('button');
    b.innerHTML = 'checked' in en ? `${icon('tick', 'tick')}<span>${esc(en.label)}</span>` : `${icon(en.icon || 'file')}<span>${esc(en.label)}</span>`;
    if (en.danger) b.className = 'danger';
    if (en.checked) b.classList.add('checked');
    b.disabled = !!en.disabled;
    if (en.title) b.title = en.title;
    b.onclick = () => { closeMenu(); en.run(); };
    menuEl.appendChild(b);
  });
  document.body.appendChild(menuEl);
  const r = menuEl.getBoundingClientRect();
  menuEl.style.left = Math.min(x, innerWidth - r.width - 8) + 'px';
  menuEl.style.top = Math.min(y, innerHeight - r.height - 8) + 'px';
}

function showMenu(side, x, y) {
  const sel = selectedItems(side);
  const one = sel.length === 1 ? sel[0] : null;
  if (side === 'local') {
    const real = sel.filter(i => !i.kind);
    openMenu(x, y, [
      one && one.is_dir ? { label: 'Mở thư mục', icon: 'folder', run: () => openItem('local', one) } : null,
      { label: real.length > 1 ? `Tải ${real.length} mục lên Google Drive` : 'Tải lên Google Drive', icon: 'up', disabled: !real.length || !S.connected, run: () => uploadSelected(real) },
      { label: 'Hiện trong File Explorer', icon: 'eye', disabled: !one || !!one.kind, run: () => api('/api/local/reveal?path=' + encodeURIComponent(one.path)) },
    ].filter(Boolean));
    return;
  }
  const trashable = S.mode !== 'link' && (S.location !== 'team' || S.drive.crumbs.length > 0);
  // Quyền drive.file: chỉ sửa/xoá/chia sẻ được mục do peaDrive tạo (API trả can_modify = false cho các mục còn lại).
  const mine = sel.every(i => i.can_modify !== false);
  const ro = mine ? '' : 'peaDrive chỉ có quyền sửa, xoá hoặc chia sẻ các mục do chính nó tạo.';
  openMenu(x, y, [
    one ? { label: one.is_dir ? 'Mở thư mục' : 'Mở trên Google Drive', icon: one.is_dir ? 'folder' : 'eye', run: () => openItem('drive', one) } : null,
    one && S.mode !== 'link' ? { label: 'Sao chép link chia sẻ', icon: 'link', disabled: !mine, title: ro, run: () => copyShareLink(one) } : null,
    { label: sel.length > 1 ? `Tải ${sel.length} mục về máy` : (one && one.is_dir ? 'Tải thư mục về máy' : 'Tải xuống'), icon: 'down', disabled: !sel.some(i => !i.is_google_doc && i.can_download !== false), run: () => downloadSelected(sel) },
    one ? { label: 'Đổi tên…', icon: 'edit', disabled: !trashable || !mine, title: ro, run: () => renameDrive(one) } : null,
    '-',
    { label: 'Chuyển vào thùng rác', icon: 'trash', danger: true, disabled: !trashable || !mine, title: ro, run: () => deleteDrive(sel, false) },
    { label: 'Xoá vĩnh viễn…', icon: 'x', danger: true, disabled: !trashable || !mine, title: ro, run: () => deleteDrive(sel, true) },
  ].filter(Boolean));
}

async function copyShareLink(item) {
  const ok = await dialog({ title: 'Tạo link chia sẻ?', message: `Bất kỳ ai có link đều xem được "${item.name}".`, ok: 'Tạo & sao chép link' });
  if (!ok) return;
  try {
    const d = await api('/api/drive/share?file_id=' + encodeURIComponent(item.id), { method: 'POST' });
    await navigator.clipboard.writeText(d.share_link).catch(() => {});
    toast('Đã sao chép link chia sẻ: ' + item.name);
  } catch (e) { toast(e.message, true); }
}

async function renameDrive(item) {
  if (S.mode === 'link') return;
  const name = await dialog({ title: 'Đổi tên', input: item.name, ok: 'Lưu' });
  if (!name || name === item.name) return;
  try { await api('/api/drive/rename', { json: { file_id: item.id, name } }); loadDrive(true); }
  catch (e) { toast(e.message, true); }
}

async function deleteDrive(items, permanent) {
  if (S.mode === 'link') return;
  if (!items.length) return;
  const ok = await dialog(permanent
    ? { title: `Xoá vĩnh viễn ${items.length} mục?`, message: 'Hành động này không thể hoàn tác. Các mục này sẽ bị xoá vĩnh viễn khỏi Google Drive để giải phóng dung lượng.', ok: 'Xoá vĩnh viễn', danger: true }
    : { title: `Chuyển ${items.length} mục vào thùng rác?`, message: 'Có thể khôi phục các mục này trong thùng rác Google Drive.', ok: 'Chuyển vào thùng rác', danger: true });
  if (!ok) return;
  try {
    await api('/api/drive/delete', { json: { file_ids: items.map(i => i.id), permanent } });
    toast(permanent ? `Đã xoá vĩnh viễn ${items.length} mục` : `Đã chuyển ${items.length} mục vào thùng rác`);
  } catch (e) { toast(e.message, true); }
  loadDrive(); loadAbout();
}

async function mkdirDrive() {
  if (S.mode === 'link') return;
  const name = await dialog({ title: 'Tạo thư mục trên Drive', input: '', ok: 'Tạo' });
  if (!name) return;
  try { await api('/api/drive/mkdir', { json: { name, parent_id: driveFolderID() } }); toast('Đã tạo thư mục ' + name); loadDrive(true); }
  catch (e) { toast(e.message, true); }
}

/* ---------------- Kéo thả giữa hai khung ---------------- */
function setupDrop(paneId, fromSide, action) {
  const pane = $(paneId);
  let depth = 0;
  const ok = e => e.dataTransfer.types.includes('application/x-peadrive') || (fromSide === 'drive' && (e.dataTransfer.types.includes('text/uri-list') || e.dataTransfer.types.includes('text/plain')));
  pane.addEventListener('dragenter', e => { if (ok(e)) { depth++; pane.classList.add('drop'); } });
  pane.addEventListener('dragleave', () => { if (--depth <= 0) { depth = 0; pane.classList.remove('drop'); } });
  pane.addEventListener('dragover', e => { if (ok(e)) { e.preventDefault(); e.dataTransfer.dropEffect = 'copy'; } });
  pane.addEventListener('drop', e => {
    e.preventDefault(); depth = 0; pane.classList.remove('drop');
    const link = (e.dataTransfer.getData('text/uri-list') || e.dataTransfer.getData('text/plain') || '').split('\n')[0].trim();
    if (e.dataTransfer.getData('application/x-peadrive') === fromSide) action();
    else if (fromSide === 'drive' && link && DRIVE_LINK.test(link)) openLinkField(link, true);
    else if (e.dataTransfer.files && e.dataTransfer.files.length) alertBox('Chưa hỗ trợ kéo từ File Explorer', 'Hãy mở thư mục đó trong khung "Máy của bạn" rồi chọn file và bấm Tải lên.');
  });
}
setupDrop('drivePane', 'local', () => uploadSelected());
setupDrop('localPane', 'drive', () => downloadSelected());
// Chặn trình duyệt mở file khi thả nhầm ra ngoài hai khung
window.addEventListener('dragover', e => e.preventDefault());
window.addEventListener('drop', e => e.preventDefault());

/* ---------------- Phím tắt ---------------- */
function keyNav(side, e) {
  const st = S[side];
  const list = visible(side);
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'a') {
    e.preventDefault(); list.forEach(i => st.sel.add(keyOf(side, i))); paintSelection(side); return;
  }
  if (e.key === 'Escape') { st.sel.clear(); paintSelection(side); return; }
  if (e.key === 'Enter') { const s = selectedItems(side); if (s.length === 1) openItem(side, s[0]); return; }
  if (e.key === 'Backspace') {
    e.preventDefault();
    if (side === 'local') { if (S.local.parent) loadLocal(S.local.parent); }
    else if (S.drive.crumbs.length) { S.drive.crumbs.pop(); loadDrive(); }
    return;
  }
  if (e.key === 'Delete' && side === 'drive') { const s = selectedItems('drive'); if (s.length) deleteDrive(s, e.shiftKey); return; }
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault();
    if (!list.length) return;
    let idx = list.findIndex(i => keyOf(side, i) === st.anchor);
    idx = e.key === 'ArrowDown' ? Math.min(list.length - 1, idx + 1) : Math.max(0, idx < 0 ? 0 : idx - 1);
    const key = keyOf(side, list[idx]);
    if (!e.shiftKey) st.sel.clear();
    st.sel.add(key); st.anchor = key;
    paintSelection(side);
    const row = document.querySelector(`#${side === 'local' ? 'localList' : 'driveList'} .row:nth-child(${idx + 1})`);
    if (row) row.scrollIntoView({ block: 'nearest' });
  }
}
$('localList').addEventListener('keydown', e => keyNav('local', e));
$('driveList').addEventListener('keydown', e => keyNav('drive', e));
// Bấm vào khoảng trống trong danh sách thì bỏ chọn
['local', 'drive'].forEach(side => $(side === 'local' ? 'localList' : 'driveList').addEventListener('click', e => {
  if (e.target.classList.contains('list')) { S[side].sel.clear(); paintSelection(side); }
}));

// Phím tắt toàn cục, khớp bảng phím tắt trên trang hướng dẫn
document.addEventListener('keydown', e => {
  if (document.querySelector('.overlay')) return;
  const k = e.key.toLowerCase();
  const typing = e.target.tagName === 'INPUT';
  if (e.key === 'F5' || (e.ctrlKey && k === 'r')) { e.preventDefault(); loadLocal(); if (S.connected) { loadDrive(true); loadAbout(); } return; }
  if (!e.ctrlKey || typing) return;
  if (k === 'u') { e.preventDefault(); uploadSelected(); }
  else if (e.shiftKey && k === 'd') { e.preventDefault(); downloadSelected(); }
  else if (k === 'o') { e.preventDefault(); loadLocal('::drives'); }
  else if (k === 'l') { e.preventDefault(); openLinkField(); }
  else if (k === 'n') { e.preventDefault(); if (!S.connected) login(); }
  else if (k === 'f') { e.preventDefault(); (document.activeElement === $('driveList') ? $('driveSearch') : $('localSearch')).focus(); }
});

document.addEventListener('paste', e => {
  if (e.target.tagName === 'INPUT' || document.querySelector('.overlay')) return;
  const text = (e.clipboardData.getData('text') || '').trim();
  if (S.connected && DRIVE_LINK.test(text)) { e.preventDefault(); openLinkField(text); }
});

/* ---------------- Nút bấm ---------------- */
$('btnDownload').onclick = () => downloadSelected();
$('btnUpload').onclick = () => uploadSelected();
$('btnMkdir').onclick = mkdirDrive;
$('btnLink').onclick = () => openLinkField();
$('sideVersion').onclick = aboutDialog;
$('btnSmartUpload').onclick = smartUploadDialog;
/* Nút Cài đặt (bánh răng): mở bảng cài đặt của bridge bất cứ lúc nào — trước đây chỉ hiện khi thiếu API (OAuth) hoặc bằng Ctrl/⌘ + , */
$('btnSettings').onclick = () => { if (typeof window.peadriveSettings === 'function') window.peadriveSettings(); };
document.querySelectorAll('.sel-toggle').forEach(b => b.onclick = () => toggleSelectAll(b.dataset.side));
// "Nhập danh sách": chọn file .txt → tự chọn các mục trùng tên trong khung tương ứng
[['local', 'localFilterTxtFile'], ['drive', 'filterTxtFile']].forEach(([side, id]) => $(id).onchange = e => {
  const f = e.target.files[0]; e.target.value = '';   // xoá giá trị để chọn lại cùng một file vẫn kích hoạt sự kiện
  importList(side, f);
});
// Đổi giao diện sáng/tối (theme ban đầu được áp bằng đoạn script nhỏ trong <head> của index.html)
$('themeToggle').addEventListener('click', () => {
  const d = document.documentElement, n = d.dataset.theme === 'light' ? 'dark' : 'light';
  d.dataset.theme = n;
  try { localStorage.setItem('theme', n); } catch (e) {}
});

/* Ô Link Drive: bấm mũi tên hoặc Enter -> truy cập link, khung Drive chuyển sang Link Mode */
function submitDriveLink() {
  const inp = $('driveLinkInput'), v = inp.value.trim();
  if (!v) return inp.focus();
  if (!DRIVE_LINK.test(v)) return toast('Link chưa đúng định dạng Google Drive', true);
  if (!S.connected) return alertBox('Chưa kết nối Google Drive', 'Vui lòng kết nối tài khoản Google Drive trước khi mở link.');
  enterLinkMode(v);
}
$('driveLinkGo').onclick = submitDriveLink;
$('driveLinkInput').addEventListener('keydown', e => { if (e.key === 'Enter' && !e.isComposing) { e.preventDefault(); submitDriveLink(); } });
$('modeMy').addEventListener('change', () => { if ($('modeMy').checked) exitLinkMode(); });
$('modeLink').addEventListener('change', () => { if ($('modeLink').checked) setTimeout(() => $('driveLinkInput').focus(), 0); });
$('btnCoffee').onclick = coffeeDialog;
$('btnThisPC').onclick = () => loadLocal('::drives');
$('btnLocalRefresh').onclick = () => loadLocal();
['local', 'drive'].forEach(side => {
  $(side === 'local' ? 'sortLocal' : 'sortDrive').onclick = e => { e.stopPropagation(); const r = e.currentTarget.getBoundingClientRect(); sortMenu(side, r.left, r.bottom + 4); };
  // Bấm tiêu đề cột: cột đang chọn thì đảo chiều, cột khác thì chuyển sang cột đó.
  document.querySelectorAll(`.list-head[data-side="${side}"] [data-sort]`).forEach(h => h.onclick = () => {
    const o = S[side].sort, key = h.dataset.sort;
    setSort(side, o.key === key ? { asc: !o.asc } : { key, asc: key === 'name' });
  });
});
$('btnDriveRefresh').onclick = () => { loadDrive(true); loadAbout(); };
$('localSearch').oninput = e => { S.local.search = e.target.value; renderList('local'); };
$('driveSearch').oninput = e => { S.drive.search = e.target.value; renderList('drive'); };
$('accountBtn').onclick = toggleAccDD;
$('sideTransfers').onclick = () => showQueue(!S.queueVisible);
$('sbShowQueue').onclick = () => showQueue(true);
$('qHide').onclick = () => showQueue(false);
$('qPause').onclick = async () => { await api(S.paused ? '/api/transfer/resume' : '/api/transfer/pause', { method: 'POST' }); pollQueue(); };
$('qCancelAll').onclick = async () => {
  if (!await dialog({ title: 'Huỷ mọi tác vụ đang chờ và đang chạy?', ok: 'Huỷ tất cả', danger: true, cancel: 'Không' })) return;
  await api('/api/transfer/cancel', { method: 'POST' }); pollQueue();
};
$('qClear').onclick = async () => { await api('/api/transfer/clear', { method: 'POST' }); lastQueueSig = ''; pollQueue(); };
$('qSmart').onclick = smartManageDialog;
document.querySelectorAll('.side-btn[data-loc]').forEach(b => b.onclick = () => {
  if (!S.connected) return login();
  exitLinkMode(true);
  S.location = b.dataset.loc; S.drive.crumbs = []; S.drive.search = ''; $('driveSearch').value = '';
  loadDrive();
});
document.addEventListener('click', e => { if (menuEl && !menuEl.contains(e.target)) closeMenu(); });
document.addEventListener('contextmenu', e => { if (!e.target.closest('.row')) { e.preventDefault(); closeMenu(); } });
window.addEventListener('blur', closeMenu);
window.addEventListener('focus', () => { if (S.local.path) loadLocal(); });

/* ---------------- Khởi động ---------------- */
showQueue(false);
refreshStatus().then(() => { renderDriveCrumbs(); if (!S.connected) renderList('drive'); });
loadLocal('');
setInterval(pollQueue, 500);
setInterval(refreshStatus, 15000);
checkUpdate();
setInterval(checkUpdate, 24 * 60 * 60 * 1000);

/* ---------------- Đa ngôn ngữ: VI / EN / ZH ----------------
   Nguồn là tiếng Việt: text/title/placeholder/aria-label khớp từ điển sẽ được đổi ngay, kể cả nội dung JS sinh ra sau đó (MutationObserver).
   Mỗi dòng: tiếng Việt|English|中文. Dòng có {} là mẫu có tham số. Tên file/thư mục (.row, .name) không bị dịch. */
let LANG = 'vi';
try { LANG = localStorage.getItem('peadrive.lang') || 'vi'; } catch (e) {}
const I18N_RAW = `Drive của tôi|My Drive|我的云端硬盘
Được chia sẻ với tôi|Shared with me|与我共享
Có gắn dấu sao|Starred|已加星标
Thùng rác|Trash|回收站
Bộ nhớ|Storage|存储空间
Bộ nhớ dùng chung|Shared storage|共享存储空间
Hàng đợi|Queue|队列
Gửi 1 ly cà phê|Buy me a coffee|请我喝杯咖啡
Kết nối Google Drive|Connect Google Drive|连接 Google Drive
Tải từ link Drive|Download from Drive link|从 Drive 链接下载
Thư mục mới|New folder|新建文件夹
Tải xuống|Download|下载
Tải lên|Upload|上传
Máy của tôi|My computer|我的电脑
Ổ đĩa|Drives|磁盘
Nhập danh sách|Import list|导入列表
Tên|Name|名称
Ngày sửa|Modified|修改时间
Dung lượng|Size|大小
Tự tiếp tục khi mở app|Resume automatically on launch|启动时自动继续
Tạm dừng tất cả|Pause all|全部暂停
Huỷ tất cả|Cancel all|全部取消
Xoá đã xong|Clear completed|清除已完成
Thao tác|Actions|操作
Nguồn|Source|来源
Đích|Destination|目标
Trạng thái|Status|状态
Tiến độ|Progress|进度
Tốc độ|Speed|速度
Còn lại|Remaining|剩余
Xem có gì mới|See what's new|查看更新内容
Bỏ qua bản này|Skip this version|跳过此版本
Để sau|Later|稍后
Tải về|Download|下载
Hiện hàng đợi|Show queue|显示队列
Ủng hộ người làm peaDrive, hoàn toàn tuỳ tâm|Support the peaDrive developer, entirely optional|支持 peaDrive 开发者，完全自愿
Thông tin ứng dụng|App info|应用信息
Đăng nhập Google Drive|Sign in to Google Drive|登录 Google Drive
Dán link file hoặc thư mục Google Drive để tải về máy (Ctrl+L)|Paste a Google Drive file or folder link to download to this PC (Ctrl+L)|粘贴 Google Drive 文件或文件夹链接，下载到本机 (Ctrl+L)
Ngôn ngữ|Language|语言
Chuyển giao diện sáng/tối|Switch light/dark theme|切换浅色/深色主题
Sáng / Tối|Light / Dark|浅色 / 深色
Xem tất cả ổ đĩa và thư mục nhanh|Browse all drives and quick folders|查看所有磁盘和常用文件夹
Sắp xếp|Sort|排序
Làm mới|Refresh|刷新
Nhập danh sách từ file .txt|Import a list from a .txt file|从 .txt 文件导入列表
Tìm trong thư mục|Search in folder|在文件夹中搜索
Chế độ Google Drive|Google Drive mode|Google Drive 模式
Dán link Google Drive|Paste a Google Drive link|粘贴 Google Drive 链接
Mở link (Enter)|Open link (Enter)|打开链接 (Enter)
Mở link|Open link|打开链接
Làm mới Google Drive|Refresh Google Drive|刷新 Google Drive
Kéo lên hoặc xuống để đổi chiều cao hàng đợi|Drag up or down to resize the queue|上下拖动以调整队列高度
Mở app sau khi tắt máy hoặc app văng: tự chạy tiếp các tác vụ chưa xong|Reopen the app after shutdown or a crash: unfinished tasks resume automatically|关机或应用崩溃后重新打开：自动继续未完成的任务
Ẩn hàng đợi|Hide queue|隐藏队列
Lỗi|Error|错误
Đồng ý|Confirm|确定
Huỷ|Cancel|取消
Đã hiểu|Got it|知道了
Ngày sửa đổi|Date modified|修改日期
Loại file|File type|文件类型
Cũ đến mới|Oldest first|从旧到新
Mới đến cũ|Newest first|从新到旧
Nhỏ đến lớn|Smallest first|从小到大
Lớn đến nhỏ|Largest first|从大到小
Sắp xếp theo|Sort by|排序方式
Chiều sắp xếp|Order|排列顺序
Thư mục|Folders|文件夹
Trên cùng|On top|置顶
Xếp lẫn với file|Mixed with files|与文件混排
{} trống|{}: empty|{}：空
Mở thư mục (hoặc nhấp đúp)|Open folder (or double-click)|打开文件夹（或双击）
Kết nối không gian của bạn|Connect your workspace|连接你的空间
Đăng nhập Google Drive để duyệt file|Sign in to Google Drive to browse files|登录 Google Drive 以浏览文件
và tải lên chỉ bằng một lần kéo thả.|and upload with a single drag and drop.|并通过一次拖放完成上传。
Đăng nhập an toàn qua Google|Secure sign-in with Google|通过 Google 安全登录
Chưa tải được thư mục|Couldn't load this folder|无法加载此文件夹
Thử lại|Try again|重试
Đang tải…|Loading…|加载中…
{} mục đã chọn: {}|{} selected: {}|已选择 {} 项：{}
{} mục|{} items|{} 项
Mở một thư mục trên máy để làm nơi lưu|Open a folder on this PC to save to|请先在本机打开一个文件夹作为保存位置
Tải {} mục về {}|Download {} items to {}|将 {} 项下载到 {}
Tải {} mục lên thư mục Drive đang mở|Upload {} items to the open Drive folder|将 {} 项上传到当前打开的 Drive 文件夹
Chọn file hoặc thư mục bên máy để tải lên|Select files or folders on this PC to upload|请选择本机的文件或文件夹进行上传
Được chia sẻ|Shared|已共享
Quản lý kết nối|Manage connection|管理连接
Đã mở trang đăng nhập Google trong trình duyệt|Opened the Google sign-in page in your browser|已在浏览器中打开 Google 登录页面
Đã kết nối Google Drive|Connected to Google Drive|已连接 Google Drive
Chuyển tài khoản|Switch account|切换账号
Đăng xuất khỏi Google Drive?|Sign out of Google Drive?|退出 Google Drive？
peaDrive sẽ xoá phiên đăng nhập trên máy này. Bạn có thể đăng nhập lại bất cứ lúc nào.|peaDrive will clear the sign-in session on this PC. You can sign in again anytime.|peaDrive 将清除本机上的登录会话，你可以随时重新登录。
Đăng xuất|Sign out|退出登录
Tài khoản|Accounts|账号
Thêm tài khoản khác|Add another account|添加其他账号
Mở Google Drive trên web|Open Google Drive on the web|在网页上打开 Google Drive
Nhà phát triển|Developer|开发者
Liên hệ|Contact|联系方式
Kiểm tra cập nhật|Check for updates|检查更新
Đang kiểm tra…|Checking…|检查中…
Bạn đang dùng bản mới nhất|You're on the latest version|当前已是最新版本
Không kiểm tra được bản cập nhật|Couldn't check for updates|无法检查更新
Chưa chọn nơi lưu|No save location selected|未选择保存位置
Hãy mở một thư mục bên khung "Máy của bạn" (ví dụ ổ D: hoặc Downloads) rồi tải về lại.|Open a folder in the "My computer" pane (e.g. drive D: or Downloads), then download again.|请先在“我的电脑”窗格中打开一个文件夹（例如 D: 盘或下载文件夹），然后重新下载。
Đã thêm {} mục vào hàng đợi tải xuống|Added {} items to the download queue|已将 {} 项加入下载队列
Chưa chọn thư mục Drive|No Drive folder selected|未选择 Drive 文件夹
Hãy mở một thư mục trên Google Drive làm nơi tải lên.|Open a folder in Google Drive to upload to.|请先在 Google Drive 中打开一个文件夹作为上传位置。
Đã thêm {} mục vào hàng đợi tải lên|Added {} items to the upload queue|已将 {} 项加入上传队列
Chưa xong từ lần trước|Unfinished from last time|上次未完成
Đang chờ|Waiting|等待中
Đang truyền|Transferring|传输中
Tạm dừng|Paused|已暂停
Hoàn tất|Completed|已完成
Đã có sẵn|Already exists|已存在
Đã huỷ|Cancelled|已取消
Tiếp tục|Resume|继续
Tác vụ đang tạm dừng|Tasks are paused|任务已暂停
Sẵn sàng|Ready|就绪
Chưa sẵn sàng|Not ready|未就绪
Chưa sẵn sàng: Chưa đăng nhập Google Drive|Not ready: Not signed in to Google Drive|未就绪：尚未登录 Google Drive
Chưa sẵn sàng: Không kết nối được tới Google Drive|Not ready: Cannot reach Google Drive|未就绪：无法连接到 Google Drive
Chưa sẵn sàng: Không liên lạc được với ứng dụng|Not ready: Cannot communicate with the app|未就绪：无法与应用通信
Không có tiến trình nào trong hàng đợi. Chọn file rồi bấm Tải lên / Tải xuống, hoặc kéo thả giữa hai khung.|No transfers in the queue. Select files and click Upload / Download, or drag and drop between the two panes.|队列中没有任务。选择文件后点击“上传 / 下载”，或在两个窗格之间拖放。
… và {} mục khác|… and {} more|… 以及另外 {} 项
Tiếp tục phiên làm việc trước?|Resume previous session?|继续上次的会话？
Bỏ các tác vụ này|Discard these tasks|放弃这些任务
Luôn tự tiếp tục|Always resume|始终自动继续
Chưa kết nối Google Drive|Google Drive not connected|尚未连接 Google Drive
Vui lòng kết nối tài khoản Google Drive trước khi tải file từ liên kết.|Please connect your Google Drive account before downloading files from a link.|从链接下载文件前，请先连接 Google Drive 账号。
Tải từ liên kết Google Drive|Download from a Google Drive link|从 Google Drive 链接下载
Dán link file hoặc thư mục Google Drive để tải thẳng về máy|Paste a Google Drive file or folder link to download straight to this PC|粘贴 Google Drive 文件或文件夹链接，直接下载到本机
LIÊN KẾT GOOGLE DRIVE|GOOGLE DRIVE LINK|GOOGLE DRIVE 链接
THƯ MỤC LƯU TRÊN MÁY|SAVE TO FOLDER ON THIS PC|本机保存文件夹
Thư mục Tải về của Windows|Windows Downloads folder|Windows 下载文件夹
Muốn lưu chỗ khác (ổ D:, E:…)? Mở thư mục đó ở khung "Máy của bạn" trước rồi bấm lại nút này.|Want to save elsewhere (drive D:, E:…)? Open that folder in the "My computer" pane first, then click this button again.|想保存到其他位置（D:、E: 盘…）？请先在“我的电脑”窗格中打开该文件夹，再点此按钮。
Đang tải {} "{}" về {}|Downloading {} "{}" to {}|正在将 {} “{}”下载到 {}
Hãy gửi 1 ly cafe để mình có thêm động lực cải tiến những chức năng tốt hơn|Buy me a coffee to give me more motivation to improve the features|请我喝杯咖啡，让我有更多动力改进功能
Quét mã QR (VietQR · Napas 247)|Scan the QR code (VietQR · Napas 247)|扫描二维码（VietQR · Napas 247）
Thôi khỏi đi|No thanks|不用了
OK luôn|Sure!|好的！
Đã sao chép số tài khoản|Account number copied|已复制账号
Đang tải bản mới trong trình duyệt|Downloading the new version in your browser|正在浏览器中下载新版本
Link này nằm trong Drive của tôi — đã mở đúng vị trí|This link is in My Drive — opened at its location|该链接位于“我的云端硬盘”中，已打开对应位置
Chủ sở hữu: {}|Owner: {}|所有者：{}
Không phải của bạn|Not owned by you|不属于您
Hiển thị|Show|显示
Chỉ mục tôi sở hữu|Only items I own|仅显示我拥有的项目
Đang lọc: chỉ mục tôi sở hữu|Filtering: only items I own|筛选中：仅我拥有的项目
Sắp xếp · đang lọc: chỉ mục tôi sở hữu|Sort · filtering: only items I own|排序 · 筛选中：仅我拥有的项目
Không có mục nào do bạn sở hữu trong thư mục này|No items you own in this folder|此文件夹中没有您拥有的项目
peaDrive {} có gì mới|What's new in peaDrive {}|peaDrive {} 更新内容
Mở thư mục|Open folder|打开文件夹
Tải {} mục lên Google Drive|Upload {} items to Google Drive|将 {} 项上传到 Google Drive
Tải lên Google Drive|Upload to Google Drive|上传到 Google Drive
Hiện trong File Explorer|Show in File Explorer|在文件资源管理器中显示
Mở trên Google Drive|Open in Google Drive|在 Google Drive 中打开
Sao chép link chia sẻ|Copy share link|复制分享链接
Tải {} mục về máy|Download {} items to this PC|将 {} 项下载到本机
Tải thư mục về máy|Download folder to this PC|将文件夹下载到本机
Đổi tên…|Rename…|重命名…
peaDrive chỉ có quyền sửa, xoá hoặc chia sẻ các mục do chính nó tạo.|peaDrive can only rename, delete or share items it created itself.|peaDrive 只能重命名、删除或分享由它自己创建的项目。
Đang chuẩn bị danh sách tải lên…|Preparing the upload list…|正在准备上传列表…
Thư mục này không do peaDrive tạo|This folder was not created by peaDrive|此文件夹不是由 peaDrive 创建的
peaDrive chỉ ghi được vào “Drive của tôi” và các thư mục do chính nó tạo (quyền drive.file), nên không tải lên thẳng vào “{}” được. Tải lên “{}” thay thế? Sau đó bạn có thể kéo các mục sang đúng chỗ trên Google Drive.|peaDrive can only write to “My Drive” and to folders it created itself (drive.file permission), so it cannot upload straight into “{}”. Upload to “{}” instead? You can drag the items to the right place on Google Drive afterwards.|peaDrive 只能写入“我的云端硬盘”以及它自己创建的文件夹（drive.file 权限），因此无法直接上传到“{}”。改为上传到“{}”吗？之后您可以在 Google 云端硬盘中把这些项目拖到正确的位置。
Tải lên thư mục thay thế|Upload to the replacement folder|上传到替代文件夹
Tất cả {} file đã có sẵn trên Drive|All {} files are already on Drive|全部 {} 个文件已在云端硬盘中
Đã tạo {} thư mục trên Drive|Created {} folders on Drive|已在云端硬盘中创建 {} 个文件夹
Đã thêm {} file vào hàng đợi tải lên|Added {} files to the upload queue|已将 {} 个文件加入上传队列
Đã thêm {} file vào hàng đợi tải lên · {} file đã có trên Drive|Added {} files to the upload queue · {} files already on Drive|已将 {} 个文件加入上传队列 · {} 个文件已在云端硬盘中
Quyền riêng tư|Privacy|隐私
Chính sách quyền riêng tư|Privacy Policy|隐私政策
peaDrive xử lý file Google Drive ngay trên máy bạn và không gửi dữ liệu Drive tới máy chủ nào của peaDrive. Việc peaDrive sử dụng, và chuyển giao cho bất kỳ ứng dụng nào khác, thông tin nhận từ Google API sẽ tuân thủ Chính sách Dữ liệu Người dùng của Dịch vụ API Google (Google API Services User Data Policy), kể cả các yêu cầu Sử dụng Hạn chế (Limited Use).|peaDrive processes your Google Drive files on your computer and does not send Drive data to any peaDrive server. peaDrive's use and transfer to any other app of information received from Google APIs will adhere to Google API Services User Data Policy, including the Limited Use requirements.|peaDrive 在您的电脑本地处理 Google 云端硬盘文件，不会将云端硬盘数据发送到任何 peaDrive 服务器。peaDrive 对从 Google API 收到的信息的使用，以及向其他应用的转移，将遵守 Google API 服务用户数据政策，包括有限使用要求。
Chuyển vào thùng rác|Move to trash|移到回收站
Xoá vĩnh viễn…|Delete permanently…|永久删除…
Tạo link chia sẻ?|Create share link?|创建分享链接？
Bất kỳ ai có link đều xem được "{}".|Anyone with the link can view "{}".|任何拥有链接的人都可以查看“{}”。
Tạo & sao chép link|Create & copy link|创建并复制链接
Đã sao chép link chia sẻ:|Share link copied:|已复制分享链接：
Đổi tên|Rename|重命名
Lưu|Save|保存
Xoá vĩnh viễn {} mục?|Delete {} items permanently?|永久删除 {} 项？
Hành động này không thể hoàn tác. Các mục này sẽ bị xoá vĩnh viễn khỏi Google Drive để giải phóng dung lượng.|This can't be undone. These items will be permanently deleted from Google Drive to free up space.|此操作无法撤销。这些项目将从 Google Drive 中永久删除以释放空间。
Xoá vĩnh viễn|Delete permanently|永久删除
Chuyển {} mục vào thùng rác?|Move {} items to trash?|将 {} 项移到回收站？
Có thể khôi phục các mục này trong thùng rác Google Drive.|You can restore these items from the Google Drive trash.|你可以在 Google Drive 回收站中恢复这些项目。
Đã xoá vĩnh viễn {} mục|Permanently deleted {} items|已永久删除 {} 项
Đã chuyển {} mục vào thùng rác|Moved {} items to trash|已将 {} 项移到回收站
Tạo thư mục trên Drive|Create folder in Drive|在 Drive 中新建文件夹
Tạo|Create|创建
Đã tạo thư mục|Folder created|已创建文件夹
Chưa hỗ trợ kéo từ File Explorer|Dragging from File Explorer isn't supported yet|暂不支持从文件资源管理器拖入
Hãy mở thư mục đó trong khung "Máy của bạn" rồi chọn file và bấm Tải lên.|Open that folder in the "My computer" pane, select the files and click Upload.|请在“我的电脑”窗格中打开该文件夹，选择文件后点击“上传”。
Link chưa đúng định dạng Google Drive|That doesn't look like a Google Drive link|链接不是有效的 Google Drive 格式
Huỷ mọi tác vụ đang chờ và đang chạy?|Cancel all pending and running tasks?|取消所有等待中和运行中的任务？
Không|No|否
Máy tính|This PC|此电脑
Drive: {} / {}|Drive: {} / {}|云端硬盘：{} / {}
Drive: {} đã dùng|Drive: {} used|云端硬盘：已用 {}
Chuyển sang {}?|Switch to {}?|切换到 {}？
peaDrive sẽ đăng xuất tài khoản hiện tại rồi mở trang đăng nhập Google. Hãy chọn đúng {} ở trang đó.|peaDrive will sign out the current account and open the Google sign-in page. Please pick {} there.|peaDrive 将退出当前账号并打开 Google 登录页面，请在该页面选择 {}。
Đã có peaDrive {}|peaDrive {} is available|peaDrive {} 已发布
Bạn đang dùng {}|You're using {}|你正在使用 {}
Đang tải lên ({}%) · còn {} file|Uploading ({}%) · {} files left|正在上传 ({}%) · 剩余 {} 个文件
Đang truyền file ({}%) · còn {} file|Transferring ({}%) · {} files left|正在传输 ({}%) · 剩余 {} 个文件
{} giây|{} sec|{} 秒
{} phút|{} min|{} 分钟
{} giờ|{} h|{} 小时
{} đã dùng|{} used|已用 {}
{}% dung lượng Drive đã dùng|{}% of Drive storage used|已使用 {}% 的 Drive 空间
Dung lượng Drive đã dùng|Drive storage used|Drive 已用空间
Dung lượng Drive|Drive storage|Drive 存储空间
Đã chuyển sang {}|Switched to {}|已切换到 {}
Đăng xuất {}?|Sign out {}?|退出 {}？
peaDrive sẽ xoá phiên đăng nhập của tài khoản này trên máy này. Các tài khoản khác vẫn giữ nguyên; tác vụ đang chạy của tài khoản này sẽ được tạm dừng.|peaDrive will remove this account's sign-in from this computer. Other accounts stay signed in; running transfers of this account will be paused.|peaDrive 将在本机移除此账号的登录状态。其他账号保持不变；此账号正在进行的传输将被暂停。
Phiên đăng nhập của {} đã hết hạn — hãy đăng nhập lại tài khoản này|The session of {} has expired — please sign in to this account again|{} 的登录已过期，请重新登录该账号
Không thể tải lên ở chế độ Link|Can't upload in Link mode|链接模式下无法上传
Chế độ Link chỉ để xem và tải xuống. Hãy chuyển về Drive của tôi để tải lên.|Link mode is view and download only. Switch back to My Drive to upload.|链接模式仅用于查看和下载。请切换回“我的云端硬盘”后再上传。
Vui lòng kết nối tài khoản Google Drive trước khi mở link.|Please connect your Google Drive account before opening a link.|打开链接前，请先连接 Google Drive 账号。
Chọn tất cả|Select all|全选
Đảo chiều vùng chọn|Invert selection|反选
Chọn một thư mục bên khung Máy của tôi để dùng Smart Upload|Select a folder in the My computer pane to use Smart Upload|请在“我的电脑”窗格中选择一个文件夹以使用 Smart Upload
Quét thư mục đã chọn và chỉ tải lên các định dạng bạn chọn|Scan the selected folder and upload only the formats you choose|扫描所选文件夹，仅上传您勾选的格式
Thư mục nguồn|Source folder|源文件夹
Đích Drive|Drive destination|Drive 目标位置
Video|Video|视频
Ảnh|Photos|图片
Định dạng khác|Other formats|其他格式
vd: braw, r3d, wav|e.g. braw, r3d, wav|例如：braw, r3d, wav
Nhập đuôi file, cách nhau bằng dấu phẩy. Không cần dấu chấm.|Enter file extensions separated by commas. No dot needed.|输入文件扩展名，用逗号分隔，无需加点。
Bắt đầu tải lên|Start upload|开始上传
Hãy kết nối Google Drive trước khi dùng Smart Upload.|Connect Google Drive before using Smart Upload.|使用 Smart Upload 前，请先连接 Google Drive。
Đã gửi Smart Upload cho {} thư mục|Smart Upload sent for {} folders|已为 {} 个文件夹发送 Smart Upload
Khung này chưa có mục nào để đối chiếu|There is nothing in this pane to match against|此窗格中没有可供匹配的项目
Không đọc được file danh sách|Couldn't read the list file|无法读取列表文件
File danh sách trống|The list file is empty|列表文件为空
Không có mục nào khớp với danh sách|No items match the list|没有与列表匹配的项目
Đã chọn {} mục khớp với danh sách|Selected {} items from the list|已按列表选中 {} 项
Đã chọn {} mục · còn {} tên chưa tìm thấy|Selected {} items · {} names not found|已选中 {} 项 · 另有 {} 个名称未找到
File chỉ được tải lên khi lần sửa đổi cuối cách hiện tại ít nhất 2 phút, để không tải file đang render dở.|Files are only uploaded when their last modification is at least 2 minutes old, so half-rendered files are never sent.|仅当文件最后一次修改距今至少 2 分钟时才会上传，因此不会上传渲染到一半的文件。
Các thư mục đang được Smart Upload theo dõi|Folders watched by Smart Upload|Smart Upload 正在监视的文件夹
Smart Upload đang chạy|Smart Upload is running|Smart Upload 正在运行
Tự tải lên file mới sau khi file không bị sửa đổi trong 2 phút (render xong)|Automatically uploads new files once they have not been modified for 2 minutes (render finished)|文件 2 分钟内未被修改（渲染完成）后自动上传
Đã tải lên {} · Đã có trên Drive {} · Chờ ổn định {}|Uploaded {} · Already on Drive {} · Waiting to settle {}|已上传 {} · Drive 上已有 {} · 等待稳定 {}
Chưa có thư mục nào đang được theo dõi|No folders are being watched|当前没有被监视的文件夹
Dừng theo dõi thư mục này?|Stop watching this folder?|停止监视此文件夹？
Các file đã vào hàng đợi vẫn tiếp tục tải lên.|Files already in the queue will keep uploading.|已在队列中的文件将继续上传。
Đã dừng Smart Upload|Smart Upload stopped|已停止 Smart Upload
Cài đặt|Settings|设置
Cài đặt (Ctrl/⌘ + ,)|Settings (Ctrl/⌘ + ,)|设置 (Ctrl/⌘ + ,)
Dừng|Stop|停止
Đóng|Close|关闭`;
const I18N = { vi: Object.create(null), en: Object.create(null), zh: Object.create(null) }, I18N_REV = Object.create(null), I18N_PAT = [];
const LANG_IDX = { vi: 0, en: 1, zh: 2 };
for (const line of I18N_RAW.trim().split('\n')) {
  const t = line.split('|');
  if (t[0].includes('{}')) I18N_PAT.push({ t, re: t.map(src => new RegExp('^' + src.replace(/[.*+?^$()|[\]\\]/g, '\\$&').replace(/\{\}/g, '(.+?)') + '$')) });
  else t.forEach((x, i) => { I18N[['vi', 'en', 'zh'][i]][t[0]] = x; if (!(x in I18N_REV)) I18N_REV[x] = t[0]; });
}
I18N_PAT.sort((a, b) => b.t[0].replace(/\{\}/g, '').length - a.t[0].replace(/\{\}/g, '').length);
function tr(v, lang = LANG) {
  const m = v.match(/^(\s*)([\s\S]*?)((?: [↑↓·])?\s*)$/), core = m[2];
  if (!core) return null;
  const hit = I18N[lang][I18N_REV[core] ?? core];
  if (hit !== undefined) return m[1] + hit + m[3];
  for (const p of I18N_PAT) for (const re of p.re) {
    const r = re.exec(core); if (!r) continue;
    let i = 0;
    return m[1] + p.t[LANG_IDX[lang]].replace(/\{\}/g, () => { const c = r[++i]; return tr(c, lang) ?? c; }) + m[3];
  }
  return null;
}
const TR_SKIP = '.row, .name, script, style';
function trAttr(n, a) { const v = n.getAttribute(a); if (!v) return; const o = tr(v); if (o != null && o !== v) n.setAttribute(a, o); }
function trNode(n) {
  if (n.nodeType === 3) {
    if (n.parentElement && n.parentElement.closest(TR_SKIP)) return;
    const o = tr(n.nodeValue); if (o != null && o !== n.nodeValue) n.nodeValue = o;
  } else if (n.nodeType === 1 && !n.closest(TR_SKIP)) {
    for (const a of ['title', 'placeholder', 'aria-label']) trAttr(n, a);
    n.childNodes.forEach(trNode);
  }
}
new MutationObserver(ms => {
  if (LANG === 'vi') return;
  for (const m of ms) {
    if (m.type === 'childList') m.addedNodes.forEach(trNode);
    else if (m.type === 'characterData') trNode(m.target);
    else if (!m.target.closest(TR_SKIP)) trAttr(m.target, m.attributeName);
  }
}).observe(document.body, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ['title', 'placeholder', 'aria-label'] });
function setLang(l) {
  LANG = I18N[l] ? l : 'vi';
  try { localStorage.setItem('peadrive.lang', LANG); } catch (e) {}
  document.documentElement.lang = LANG;
  $('uiLang').value = LANG;
  trNode(document.body);
}
$('uiLang').onchange = e => setLang(e.target.value);
setLang(LANG);

pollQueue();
checkSession();
