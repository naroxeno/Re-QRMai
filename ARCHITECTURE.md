# Re-QRMai 项目架构文档

> 版本：0.1.0 ｜ 更新日期：2026-08-15 ｜ 语言：Rust (edition 2024)

本文档描述 Re-QRMai 的整体架构、核心业务流程、模块划分与关键技术决策，
供开发者快速理解代码结构并参与后续开发。

---

## 1. 项目概述

Re-QRMai 是一个用 **Rust** 从零重写的「Re-QRMai」工具（原版为 Node.js 项目），
核心目标是**自动化获取音游 MAI 微信登录二维码**：

1. 用户通过浏览器访问本工具提供的二维码页面（默认 `/qrmai`）；
2. 服务端**模拟鼠标点击**微信窗口中的「生成二维码」按钮（P1）与二维码消息区域（P2）；
3. 微信随后通过系统 `xdg-open` 打开一个形如
   `https://wq.wahlap.net/qrcode/req/MAIDxxxx.html` 的链接；
4. 该链接被**劫持 / 扩展**两种方式之一捕获，服务端据此拿到 MAID 页面地址；
5. 服务端抓取页面 HTML → 提取二维码图片地址 → 下载图片 → **解码出二维码内容**；
6. 服务端将二维码内容重新编码为 PNG 返回给浏览器展示，用户扫码完成登录。

一句话概括：**一个把「微信内点击 + 浏览器扫码」手工流程自动化的本地 Web 服务**。

---

## 2. 总体架构图

```
┌────────────────────────────────────────────────────────────────┐
│                        用户浏览器                               │
│   GET /qrmai ──► 展示二维码 PNG        /settings 管理面板        │
└──────────┬───────────────────────────────────────────────────────┘
           │ HTTP
┌──────────▼───────────────────────────────────────────────────────┐
│                    Re-QRMai 二进制 (Rocket 服务)                  │
│                                                                  │
│  ┌──────────┐   ┌────────────┐   ┌────────────┐  ┌────────────┐ │
│  │ main.rs  │──►│ wechat.rs  │──►│ mouse.rs   │  │ detect.rs  │ │
│  │ 路由/配置 │   │ 微信劫持    │   │ 鼠标控制    │  │ 截图+模板匹配│ │
│  └──────────┘   └────────────┘   └────────────┘  └────────────┘ │
│       │                │                │              │         │
│       │        ┌───────▼────────┐  ┌───▼────┐   ┌─────▼──────┐   │
│       │        │ FIFO / 伪 xdg- │  │ enigo  │   │ grim-rs /  │   │
│       │        │ open (Linux)   │  │ 模拟鼠标│   │ windows-   │   │
│       │        └────────────────┘  └────────┘   │ capture /  │   │
│       │                                         │ screencap- │   │
│       │                                         │ turekit    │   │
│       │                                         └────────────┘   │
│       │                                                        │
│  ┌────▼───────────────────────────────────────────────────┐     │
│  │ 数据链路（两条，按 capture_mode 切换）                   │     │
│  │                                                        │     │
│  │ A. 劫持模式 (Linux)                                    │     │
│  │    微信 ──xdg-open──► 伪脚本 ──FIFO──► 监听线程 ──► URL  │     │
│  │                                                        │     │
│  │ B. 扩展模式 (跨平台)                                    │     │
│  │    微信 ──xdg-open──► 浏览器 ──扩展拦截──► POST /url ──►   │     │
│  │                       QR 缓存 (QrCache) ◄──写入        │     │
│  └────────────────────────────────────────────────────────┘     │
└──────────────────────────────────────────────────────────────────┘
```

---

## 3. 技术栈

| 领域 | 选型 | 说明 |
|------|------|------|
| Web 框架 | **Rocket 0.5** | 异步 HTTP 服务，路由 + 状态管理 |
| 模板引擎 | **minijinja 2** | 设置页服务端渲染（settings.html） |
| 鼠标控制 | **enigo 0.6** | 跨平台模拟鼠标移动/点击 |
| 光标位置读取 | hyprctl / xdotool（外部命令） | Linux 下读取当前光标坐标 |
| 模板匹配 | **template-matching 0.2** | GPU 加速 SSD 模板匹配，替代 OpenCV |
| 屏幕截图 | grim-rs (Linux) / windows-capture (Windows) / screencapturekit (macOS) | 按平台条件编译 |
| 二维码解码 | **zedbar 0.4** | 纯 Rust 条码/二维码解码 |
| 二维码编码 | **qrcode 0.14** | 将解码结果重新编码为 PNG |
| HTTP 客户端 | **ureq 3** | 同步请求 MAID 页面与二维码图片 |
| 日志 | **flexi_logger** | 彩色终端输出 + 按天分文件 |
| 配置 | serde + toml | config.toml 读写（带注释模板渲染） |
| 错误处理 | anyhow | 上下文错误链 |
| 浏览器扩展 | 原生 JS (MV3 / MV2) | Chrome 与 Firefox 双版本 |

---

## 4. 目录结构

```
Re-QRMai/
├── Cargo.toml               # 依赖与编译配置（release 极致瘦身）
├── build.nu                 # Nushell 构建/打包脚本
├── config.toml              # 运行时配置（TOML，自动生成带中文说明，启动时加载）
├── src/
│   ├── main.rs              # 入口：Rocket 路由、配置、日志、QR 处理
│   ├── mouse.rs             # MouseController：鼠标移动/点击 + 光标位置后端
│   ├── wechat.rs            # WechatHijack：Linux 微信劫持 + QR 解码/抓取
│   └── detect.rs            # 跨平台截图 + P1/P2 多尺度模板匹配
├── templates/
│   ├── login.html           # 登录页（静态，编译期嵌入二进制）
│   └── settings.html        # 设置页（minijinja 模板，编译期嵌入）
├── extension/               # 浏览器扩展（跨平台 QR 捕获方案）
│   ├── manifest.json        # Chrome MV3 清单
│   ├── manifest.firefox.json# Firefox MV2 清单
│   ├── background.js        # 导航拦截 + 转发 URL 到服务端
│   ├── options.html/.js     # 扩展配置页
│   └── icon.png
├── img/                     # 模板图 / 皮肤（运行时生成默认模板）
│   ├── p1.png / p2.png      # 开发者默认模板（编译期嵌入，按需写出）
│   └── p1_user.png / ...    # 用户上传模板（优先级更高）
├── log/                     # 运行日志（按天 + 序号分文件）
└── dist/                    # 构建产物（二进制 / 扩展 zip / 发布包）
```

---

## 5. 模块详解

### 5.1 `src/main.rs` — 入口与 Web 层

职责：
- **配置加载**：`load_or_create_config()` 读取 `config.toml`，不存在则生成带说明的默认配置；
  运行时用 `Arc<RwLock<Config>>`（`SharedConfig`）共享，支持热更新。
- **静态资源自举**：`ensure_img_dir()` 确保 `img/` 目录存在，并将编译期
  嵌入的默认模板（`include_bytes!`）写出，实现单二进制分发。
- **Rocket 路由**：见第 7 节 API 清单。
- **日志初始化**：`init_logger()` 按 `YYYY-MM-DD-N` 规则生成当天第 N 个日志文件，
  终端彩色输出 + 文件双写（见第 9 节）。
- **QR 获取编排**：`qrmai_handler` 根据 `capture_mode` 分流到
  「劫持模式」或「扩展模式」，鼠标点击在 `spawn_blocking` 线程池执行，避免阻塞异步运行时。
- **QR 编码**：`qr_png_response()` 将解码得到的字符串用 `qrcode` 重新编码为 PNG。

关键共享状态：

| 类型 | 说明 |
|------|------|
| `SharedConfig` | `Arc<RwLock<Config>>`，异步锁，配置热更新 |
| `QrCache` | `Arc<RwLock<Option<(String, Instant)>>>`，扩展模式缓存最新解码结果 |
| `HijackState` | `Option<Arc<Mutex<WechatHijack>>>`，Linux 劫持环境（std Mutex，跨线程） |

### 5.2 `src/mouse.rs` — 鼠标控制

- **MouseController**：封装 `enigo`，提供 `move_to` / `click` / `move_click`（带延迟）。
- **光标位置后端**：`PosBackend` 枚举自动探测：
  - `Hyprctl`（Wayland/Hyprland）优先；
  - `Xdotool`（X11）次之；
  - 均不可用则返回 `None`。
- 位置读取用于设置页「自动识别位置」功能（`GET /mouse_position`）。

### 5.3 `src/wechat.rs` — Linux 微信劫持（核心模块）

这是整个工具最有技术含量的模块，实现「截获微信打开的 URL」：

1. **伪装 xdg-open**：在临时目录创建可执行的 `xdg-open` 脚本，
   若参数匹配 `wq.wahlap.net/qrcode/req/MAID*.html` 则把 URL 写入 **FIFO 管道**，
   否则转发给系统 `/usr/bin/xdg-open`（不破坏正常打开行为）。
2. **FIFO 监听线程**：`spawn_fifo_listener` 后台线程持续读取管道，
   通过 `mpsc::channel` 把 URL 传给主流程。
3. **微信进程管理**：
   - `launch_wechat`：以 `dbus-run-session` + 修改 `PATH`（把伪装目录置前）启动微信；
   - `is_wechat_alive`：基于 PID（`libc::kill(pid, 0)`）探活。
4. **崩溃恢复**：环境状态（微信 PID、FIFO 路径、伪装目录）持久化到
   `/tmp/qrmai_state.json`，下次启动 `try_recover()` 优先复用已存在的劫持环境，
   无需重启微信；清理时支持 `keep=true` 保留环境以便快速恢复。
5. **QR 获取与解码**：
   - `fetch_and_decode(url)`：`ureq` 请求 MAID 页面 → 正则提取
     `<img src="...MAID...png">`（带 fallback 到任意第一个 img）→ 下载图片；
   - `decode_qr_from_bytes`：`zedbar` 纯 Rust 解码二维码。
6. **扫码动作 `qr_action`**：点击 P1 → 等待 2s → 点击 P2（至多 2 次尝试）→
   等待 FIFO 收到 URL → 抓取并解码。P2 点击两次（一次可能因 UI 状态失败）。

### 5.4 `src/detect.rs` — 截图与位置自动识别

- **跨平台截图**：统一输出 f32 灰度 `ImageBuffer`：
  - Linux：`grim-rs`（Wayland 截图）；
  - Windows：`windows-capture`（GraphicsCapturePicker 单帧捕获）；
  - macOS：`screencapturekit`（SCScreenshotManager）。
- **多尺度模板匹配**：`match_template_multiscale` 对模板按
  `[0.6, 0.8, 1.0, 1.2, 1.5]` 缩放，用 `template-matching`（SSD，GPU 加速）匹配，
  将 SSD 最小值归一化为置信度 `1 - min/template_area`。
- **P1/P2 选取策略**：P1 取「最高置信度」(`best`)，P2 取「Y 坐标最大」(`bottom`)，
  因为二维码消息通常出现在窗口底部。
- **模板优先级**：`p1_user.png` > `p1.png`（用户模板优先于开发者默认模板）。

---

## 6. 两种 QR 捕获模式

通过配置项 `capture_mode` 切换（Linux 默认 `hijack`，其他平台默认 `extension`）：

### 6.1 劫持模式（Linux only）

```
GET /qrmai
  → spawn_blocking: WechatHijack::qr_action()
      → 点击 P1（生成二维码按钮）
      → 点击 P2（二维码消息，触发微信打开链接）
      → 微信调用 PATH 中的伪 xdg-open → URL 写入 FIFO
      → FIFO 监听线程收到 URL → 返回
  → fetch_and_decode(URL) → 解码字符串
  → qrcode 编码 → PNG 返回浏览器
```

优点：无需浏览器扩展；缺点：仅 Linux，需要微信通过伪装脚本启动。

### 6.2 扩展模式（跨平台）

```
GET /qrmai
  → 清空 QrCache
  → spawn_blocking: click_p1p2() 点击 P1 → P2（不做 FIFO 等待）
  → 浏览器被微信唤起并打开 MAID 链接
  → 扩展 background.js 的 webNavigation 监听命中
      → 立即关闭标签页
      → POST {url, token} → /qrmai/url
  → 服务端 /qrmai/url：校验 token → fetch_and_decode → 写入 QrCache → 返回 PNG
  → /qrmai 轮询 QrCache（200ms 间隔，超时 wechat_url_timeout 秒）→ 命中后返回 PNG
```

优点：全平台通用（Windows / macOS / Linux 均可）；缺点：依赖浏览器扩展。

---

## 7. Web API

| 方法 | 路径 | 鉴权 | 说明 |
|------|------|------|------|
| GET | `/`、`/login` | 无 | 登录页（静态 HTML，编译期嵌入） |
| POST | `/login` | 无 | 校验 token，成功则写入私有加密 Cookie `auth_token` |
| GET | `/settings` | Cookie | 设置页（minijinja 渲染，含平台信息 `is_linux`） |
| POST | `/settings` | Cookie | 保存设置：更新内存 Config 并写回 `config.toml`（表单逐字段映射，渲染带注释模板） |
| GET | `/mouse_position` | 无 | 返回当前光标坐标（设置页「自动识别位置」） |
| POST | `/detect_positions` | 无 | 截图 + 模板匹配，返回识别出的 P1/P2 坐标 |
| GET | `{qr_route}`（默认 `/qrmai`） | 无 | 核心：点击 + 捕获 + 解码 + 返回二维码 PNG |
| POST | `{qr_route}/url` | JSON token | 扩展模式：提交捕获的 URL，解码后写入缓存并返回 PNG |
| GET | `/img/*` | 无 | 静态文件服务（模板图 / 皮肤） |
| GET | `/extension/*` | 无 | 静态文件服务（扩展安装包可直接托管） |

---

## 8. 配置系统

- 存储：`config.toml`（服务根目录），由 `Config` 结构体（serde）驱动；
  启动时不存在则**自动生成带中文说明的默认 TOML**（`render_config_toml`，注释即文档）。
- 格式：TOML（人类易读、支持注释）；旧版 `config.json` 不再读取（无自动迁移，可手动迁移）。
- 热更新：`SharedConfig = Arc<RwLock<Config>>`，设置页保存后内存与文件同步更新
  （保存时按模板重新生成带注释的 TOML，注释常驻）。
- 主要配置项：

| 配置 | 默认值 | 说明 |
|------|--------|------|
| `p1` / `p2` | [1892,1407] / [1453,1300] | 微信窗口点击坐标（可用自动识别覆盖） |
| `token` | qrmai | 管理面板登录令牌 + 扩展提交鉴权 |
| `host` / `port` | 0.0.0.0 / 5000 | 服务监听地址 |
| `qr_route` | /qrmai | 二维码页面路由前缀 |
| `capture_mode` | 平台相关 | `hijack`（Linux）/ `extension`（其他） |
| `decode.time` / `retry_count` | 10 / 10 | 解码参数（预留） |
| `wechat_bin` / `wechat_url_timeout` | /opt/wechat/wechat / 5 | 微信路径与 URL 等待超时 |
| `auto_detect_p1p2` / `template_threshold` | false / 0.8 | 启动自动识别开关与置信度阈值 |
| `skin_mode` / `skin_index` / `skin_images` | random / 0 / [] | 二维码皮肤（展示样式） |
| `custom_skin_path` / `custom_skin_qrcode_size` / `custom_skin_qrcode_point` | ./skin.png / 576 / [106,638] | 自定义皮肤与二维码嵌入位置 |
| `standalone_mode` / `cache_duration` | false / 0 | 独立模式 / 二维码缓存（预留） |
| `dev_mode` | false | 开发模式 |
| `version` | 哈希串 | 版本标识（预留） |

---

## 9. 日志系统

- 基于 `flexi_logger`，**双写**：彩色终端（stderr）+ 文件（`log/` 目录）。
- 文件名规则：`YYYY-MM-DD-N.log`，N 为当天运行序号（启动时扫描 `log/` 目录自增），
  避免同日多次运行互相覆盖。
- 终端格式 `[HH:MM:SS] LEVEL msg`；文件格式 `[YYYY-MM-DD HH:MM:SS] LEVEL msg`。

---

## 10. 浏览器扩展（extension/）

- **双版本**：`manifest.json`（Chrome MV3 service worker）与
  `manifest.firefox.json`（Firefox MV2 background.scripts），共用 `background.js`。
- **工作流程**（background.js）：
  1. `webNavigation.onBeforeNavigate` 监听导航，过滤非顶层 frame；
  2. `QR_PATTERNS` 正则匹配 wahlap MAID 类链接；
  3. 立即 `tabs.remove` 关闭标签页（不等异步）；
  4. 读取 `storage.sync` 配置（服务端地址/端口/路由/token），
     fire-and-forget POST 到 `{qr_route}/url`。
- **配置页**：options.html/options.js 管理服务端地址、token、是否关闭标签页、通知开关。

---

## 11. 跨平台支持策略

| 能力 | Linux | Windows | macOS |
|------|-------|---------|-------|
| 截图 | grim-rs | windows-capture | screencapturekit |
| 微信劫持 | ✅（伪 xdg-open + FIFO） | ❌（用扩展模式） | ❌（用扩展模式） |
| 鼠标控制 | enigo | enigo | enigo |
| 默认 capture_mode | hijack | extension | extension |
| 链接器 | mold / wild | 默认 | 默认 |

平台差异全部通过 `#[cfg(target_os = "...")]` 条件编译隔离，
`capture_screen()` 在各平台提供同名统一接口（见 detect.rs）。

---

## 12. 构建与发布

- 编译配置（Cargo.toml）：
  - `release`：`opt-level = "z"` + `lto = "fat"` + `strip = "symbols"` +
    `panic = "abort"` + `codegen-units = 1`，追求最小体积（单二进制分发）；
  - `dev`：`debug = "line-tables-only"`，依赖不调试以加速编译；
  - `debugging`：继承 dev 但全量调试符号。
- 打包脚本 `build.nu`（Nushell）：
  - `nu build.nu`（debug）/ `nu build.nu release` / `nu build.nu extension` / `nu build.nu all`；
  - 产物：`dist/Re-QRMai`（可执行）、`extension-chrome.zip`、
    `extension-firefox.zip`、`qrmai-rs-v{version}-{os}-{arch}.tar.gz`（完整发布包）。

---

## 13. 关键设计决策

1. **单二进制分发**：登录页/设置页模板与默认模板图均通过 `include_str!` /
   `include_bytes!` 编译进二进制，运行时按需写出，用户无需额外素材即可启动。
2. **异步非阻塞**：所有阻塞操作（鼠标点击、FIFO 等待、网络请求、QR 解码）统一
   放入 `spawn_blocking` 线程池，Rocket 异步运行时保持响应。
3. **纯 Rust 解码**：用 `zedbar` 替代 zbar 系统库，免去外部动态库依赖，跨平台分发简单。
4. **GPU 模板匹配替代 OpenCV**：`template-matching` 更轻量，且天然跨平台。
5. **劫持可恢复**：环境状态持久化到 `/tmp`，崩溃后无需重建 FIFO / 伪装脚本 / 重启微信。
6. **两种捕获模式并存**：Linux 优先原生劫持（零依赖），其他平台退化为浏览器扩展方案，
   同一套 `fetch_and_decode` 解码管线被两条链路复用。
7. **配置热更新**：设置页保存即写 `config.toml`（带注释模板）并更新内存锁，无需重启服务。

---

## 14. 数据流时序（劫持模式，一次完整请求）

```
浏览器                Rocket               spawn_blocking              微信/系统
  │  GET /qrmai         │                        │                      │
  ├────────────────────►│                        │                      │
  │                     │ qr_action() ──────────►│ 点击 P1              │
  │                     │                        ├─────────────────────►│
  │                     │                        │ 点击 P2（触发打开链接）│
  │                     │                        ├─────────────────────►│
  │                     │                        │         伪 xdg-open   │
  │                     │                        │◄─────────────────────┤
  │                     │                        │ FIFO 收到 URL         │
  │                     │                        │ fetch_and_decode()    │
  │                     │                        │  抓 HTML→提取 img→下载 │
  │                     │◄───────────────────────┤  zedbar 解码 QR 内容  │
  │ ◄── PNG ────────────┤                        │                      │
```

---

## 15. 已知边界与后续方向（供参考）

- `decode.time` / `retry_count` / `cache_duration` / `standalone_mode` /
  `dev_mode` 等配置当前为预留字段，未完全生效；
- 扩展模式依赖本地 HTTP（`host_permissions` 限定 localhost），若服务端 token 泄露
  存在本地越权风险，建议后续支持 HTTPS 或绑定回环地址；
- 劫持模式强依赖 Linux + `xdg-open` 环境（`dbus-run-session` 启动微信），
  Wayland/X11 不同合成器下的鼠标点击与截图行为有待进一步适配验证；
- 后续可考虑：皮肤系统（`skin_*`）的完整渲染、二维码缓存策略落地、
  自动识别（`auto_detect_p1p2`）开机自检、CI 自动化构建发布等。
