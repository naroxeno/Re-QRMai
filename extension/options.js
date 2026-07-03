// ── QRMai Bridge — Options Page (Chrome + Firefox) ──────

// 跨浏览器 storage API 封装（兼容 Chrome MV3 / Firefox MV2）
function storageGet(keys) {
  return new Promise((resolve) => chrome.storage.sync.get(keys, resolve));
}
function storageSet(items) {
  return new Promise((resolve) => chrome.storage.sync.set(items, resolve));
}

const DEFAULT_CONFIG = {
  serverHost: '127.0.0.1',
  serverPort: 5000,
  qrRoute: '/qrmai',
  token: 'qrmai',
  closeTab: true,
  showNotification: true
};

// 加载已保存配置
async function loadConfig() {
  const stored = await storageGet(Object.keys(DEFAULT_CONFIG));
  const config = { ...DEFAULT_CONFIG, ...stored };
  document.getElementById('serverHost').value = config.serverHost;
  document.getElementById('serverPort').value = config.serverPort;
  document.getElementById('qrRoute').value = config.qrRoute;
  document.getElementById('token').value = config.token;
  document.getElementById('closeTab').checked = config.closeTab;
  document.getElementById('showNotification').checked = config.showNotification;
}

// 保存配置
async function saveConfig() {
  const config = {
    serverHost: document.getElementById('serverHost').value.trim() || '127.0.0.1',
    serverPort: parseInt(document.getElementById('serverPort').value) || 5000,
    qrRoute: document.getElementById('qrRoute').value.trim() || '/qrmai',
    token: document.getElementById('token').value.trim() || 'qrmai',
    closeTab: document.getElementById('closeTab').checked,
    showNotification: document.getElementById('showNotification').checked
  };

  try {
    await storageSet(config);
    showStatus('设置已保存', 'success');
  } catch (err) {
    showStatus('保存失败: ' + err.message, 'error');
  }
}

function showStatus(msg, type) {
  const status = document.getElementById('status');
  status.textContent = msg;
  status.className = type;
  setTimeout(() => { status.textContent = ''; status.className = ''; }, 2000);
}

document.getElementById('save').addEventListener('click', saveConfig);
document.addEventListener('DOMContentLoaded', loadConfig);
