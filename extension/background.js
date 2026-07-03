// ── QRMai Bridge — Background Script (Chrome + Firefox) ─────
//
// 监听浏览器导航事件，当外部应用（如微信）打开符合二维码链接特征的
// URL 时，立即关闭标签页并将链接转发给本地 QRMai 服务端。
//
// 兼容：Chrome MV3 (service_worker) / Firefox MV2 (background.scripts)

// ── 跨浏览器 API 封装 ──────────────────────────────────

function storageGet(keys) {
  return new Promise((resolve) => chrome.storage.sync.get(keys, resolve));
}

// ── 默认配置 ────────────────────────────────────────────

const DEFAULT_CONFIG = {
  serverHost: '127.0.0.1',
  serverPort: 5000,
  qrRoute: '/qrmai',
  token: 'qrmai',
};

// ── 二维码链接正则 ──────────────────────────────────────

const QR_PATTERNS = [
  /https?:\/\/wq\.wahlap\.net\/qrcode\/req\/MAID[0-9A-Fa-f]+\.html/,
  /https?:\/\/maimai\.wahlap\.com\/.*MAID.*/,
  /https?:\/\/chunithm\.wahlap\.com\/.*MAID.*/
];

// ── URL 匹配 ────────────────────────────────────────────

function isQRUrl(url) {
  return QR_PATTERNS.some(p => p.test(url));
}

// ── 导航拦截 ────────────────────────────────────────────
//
// onBeforeNavigate 在页面加载前触发。我们立即关闭标签页，
// 然后异步将 URL 转发给服务端（fire-and-forget）。

chrome.webNavigation.onBeforeNavigate.addListener((details) => {
  if (details.frameId !== 0) return;
  const url = details.url;
  if (!isQRUrl(url)) return;

  console.log('[QRMai Bridge] 拦截到二维码链接:', url);

  // 1) 立刻关闭标签页，不等任何异步操作
  chrome.tabs.remove(details.tabId, () => {
    if (chrome.runtime.lastError) {
      // 标签页可能已经被关闭，忽略错误
    }
  });

  // 2) 异步发送 URL 到服务端（fire-and-forget）
  storageGet(Object.keys(DEFAULT_CONFIG)).then((stored) => {
    const config = { ...DEFAULT_CONFIG, ...stored };
    const serverUrl =
      `http://${config.serverHost}:${config.serverPort}${config.qrRoute}/url`;

    return fetch(serverUrl, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ url, token: config.token })
    });
  }).then((resp) => {
    if (resp && resp.ok) {
      console.log('[QRMai Bridge] 链接已转发到服务端');
    }
  }).catch((err) => {
    console.error('[QRMai Bridge] 无法连接服务端:', err.message);
  });
});

console.log('[QRMai Bridge] 后台脚本已启动');
