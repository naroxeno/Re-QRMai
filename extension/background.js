// ── QRMai Bridge — Background Script (Chrome + Firefox) ─────
//
// 监听浏览器导航事件，当外部应用（如微信）打开符合二维码链接特征的
// URL 时：① 在请求发出前直接取消（不访问 wahlap 服务器、不加载页面），
// ② 关闭标签页，③ 将链接转发给本地 QRMai 服务端。
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

// ── 请求拦截：不让链接被加载 ────────────────────────────
//
// MV3：declarativeNetRequest 动态规则 block（请求在发出前被取消）
// MV2：webRequest.onBeforeRequest 返回 cancel:true
// 二者都保证请求不会访问 wahlap 服务器。

function installRequestBlocker() {
  // ── Chrome MV3：declarativeNetRequest ──
  if (chrome.declarativeNetRequest) {
    const rules = [
      { id: 1, urlFilter: '||wq.wahlap.net/qrcode/req/MAID', resourceTypes: ['main_frame'] },
      { id: 2, urlFilter: '||maimai.wahlap.com/*MAID*', resourceTypes: ['main_frame'] },
      { id: 3, urlFilter: '||chunithm.wahlap.com/*MAID*', resourceTypes: ['main_frame'] }
    ].map(r => ({
      id: r.id,
      priority: 1,
      action: { type: 'block' },
      condition: { urlFilter: r.urlFilter, resourceTypes: r.resourceTypes }
    }));
    chrome.declarativeNetRequest.updateDynamicRules({
      removeRuleIds: rules.map(r => r.id),
      addRules: rules
    }).then(() => {
      console.log('[QRMai Bridge] declarativeNetRequest 拦截规则已安装');
    }).catch((err) => {
      console.warn('[QRMai Bridge] DNR 规则安装失败:', err && err.message);
    });
    return;
  }

  // ── Firefox MV2：webRequest cancel ──
  if (chrome.webRequest && chrome.webRequest.onBeforeRequest) {
    chrome.webRequest.onBeforeRequest.addListener(
      (details) => {
        if (isQRUrl(details.url)) {
          console.log('[QRMai Bridge] webRequest 取消请求:', details.url);
          return { cancel: true };
        }
      },
      { urls: ['<all_urls>'] },
      ['blocking']
    );
    console.log('[QRMai Bridge] webRequest 拦截已安装');
  }
}

// ── 已处理的 URL（防止 onBeforeNavigate / onErrorOccurred 重复触发） ──

const processed = new Set();

function handleQRUrl(url, tabId) {
  if (!isQRUrl(url)) return;
  if (processed.has(url)) return;
  processed.add(url);

  console.log('[QRMai Bridge] 拦截到二维码链接:', url);

  // 1) 关闭标签页（若请求被取消，标签页可能停在错误页，一并关闭）
  if (tabId && tabId > 0) {
    chrome.tabs.remove(tabId, () => {
      if (chrome.runtime.lastError) {
        // 标签页可能已被关闭，忽略错误
      }
    });
  }

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
}

// ── 导航拦截（兜底：请求被取消后标签页可能停在错误页） ──

chrome.webNavigation.onBeforeNavigate.addListener((details) => {
  if (details.frameId !== 0) return;
  handleQRUrl(details.url, details.tabId);
});

// DNR/webRequest 取消请求后，onBeforeNavigate 可能不触发，
// 标签页会进入错误页（onErrorOccurred）——在这里兜底关闭并转发。
if (chrome.webNavigation.onErrorOccurred) {
  chrome.webNavigation.onErrorOccurred.addListener((details) => {
    if (details.frameId !== 0) return;
    handleQRUrl(details.url, details.tabId);
  });
}

// ── 启动 ────────────────────────────────────────────────

installRequestBlocker();
console.log('[QRMai Bridge] 后台脚本已启动');
