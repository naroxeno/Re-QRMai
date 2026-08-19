# QRMai-rs

自动获取音游 MAI（舞萌）微信登录二维码的本地工具。模拟鼠标点击微信窗口生成二维码，
劫持/拦截微信打开的 MAID 链接，抓取并解码后以二维码页面形式返回，扫码即可登录。

Rust 重写版（Re-QRMai），单二进制分发。

> ⚠️ **安全警告**：首次启动会自动生成随机访问令牌（`qrmaiXXXXXX` 格式，见 `config.toml`），
> 但默认 `host = "0.0.0.0"`（局域网可访问）。**部署到任何可被他人访问的网络前，
> 请确认 `config.toml` 中的 token 已改为随机值**（旧版 `config.toml` 若仍是 `qrmai` 请手动修改）。

## 功能特性

- 🔄 自动点击微信生成/展示二维码（P1/P2 坐标可配置，支持模板匹配自动识别）
- 🪝 Linux 劫持模式：伪装 `xdg-open` + FIFO 拦截微信打开的 MAID 链接（零依赖）
- 🧩 浏览器扩展模式：拦截链接（请求级取消，不加载页面），跨平台可用
- 📸 模板匹配自动识别 P1/P2 位置（GPU 加速，检测区按屏幕分辨率自适应）
- 🎨 现代设置界面（Tailwind + Nord 配色，深/浅色主题）
- 📝 TOML 配置，自动生成带中文说明的配置文件

## 平台支持

| 平台 | 截图 | 二维码捕获 | 匹配 | 0.1.0 状态 |
|---|---|---|---|---|
| Linux | ✅ grim-rs | ✅ 劫持 / 扩展 | ✅ GPU | ✅ 支持 |
| Windows | ✅ windows-capture | ✅ 扩展 | ✅ GPU | ✅ 支持（请在 Windows 本机构建） |
| macOS | ✅ screencapturekit | ✅ 扩展 | ✅ GPU | 🚧 **0.1.0 暂不提供**（构建依赖需在本机处理，见下文） |

> macOS：由于 `template-matching`（wgpu 0.16）的 ObjC 依赖链在 Linux 交叉编译到 macOS 时脆弱，
> 0.1.0 未包含 macOS 产物。如需 macOS 版本，请在 Mac 上执行 `cargo build --release` 本机构建。

## 快速开始

### 1. 启动服务

```bash
./QRMai-rs          # 或 cargo run --release
```

首次启动自动生成带说明的 `config.toml`（默认端口 5000）。Linux 下还会尝试启动微信劫持环境。

### 2. 打开管理面板

浏览器访问 `http://127.0.0.1:5000`，使用 `config.toml` 中的令牌登录（首次启动自动生成的随机值）。

### 3. 配置 P1/P2 坐标

- **手动**：设置页「位置设置」填入 P1（生成二维码按钮）与 P2（二维码消息）坐标，格式 `X,Y`；
- **自动**：上传 P1/P2 模板截图到「模板图片」，点击「🔍 自动识别位置」，识别结果自动填入；
- 多显示器/高分屏：可调整「P2 检测区」比例（自动适配分辨率）或手动像素值。

### 4. 获取二维码

浏览器访问 `http://127.0.0.1:5000/qrmai`，服务端自动点击微信生成二维码并返回二维码页面，扫码登录。

## 二维码捕获方式

### Linux 劫持模式（默认，零依赖）

1. 设置页将「捕获模式」设为 `xdg-open 劫持`；
2. 服务启动时自动创建伪装 `xdg-open` 并启动微信（`wechat_bin` 路径可配置）；
3. 微信打开 MAID 链接时被拦截，无需安装任何东西。

### 浏览器扩展模式（Windows 推荐 / 跨平台）

1. 设置页将「捕获模式」设为 `浏览器扩展`；
2. 浏览器开发者模式 →「加载已解压的扩展」→ 选择 `extension/` 目录（Chrome MV3 / Firefox MV2 双版本）；
3. 扩展选项页确认服务端地址（`127.0.0.1:5000`）与令牌；
4. 微信打开的 MAID 链接会被扩展**在请求发出前取消**并转发给服务端，标签页瞬间关闭。

> 修改 `port` / `qr_route` 后需同步扩展选项页。

## 配置说明

配置文件为 `config.toml`（首次启动自动生成，含中文注释即文档）。常用项：

| 配置 | 默认 | 说明 |
|---|---|---|
| `token` | 随机生成 | 登录令牌 + 扩展鉴权（首次启动自动生成 `qrmaiXXXXXX`，也可手动改） |
| `host` / `port` | 0.0.0.0 / 5000 | 监听地址（127.0.0.1 = 仅本机） |
| `capture_mode` | 平台相关 | `hijack`（Linux）/ `extension`（其他） |
| `p1` / `p2` | 屏幕坐标 | 微信点击位置（可用自动识别覆盖） |
| `template_threshold` | 0.8 | 模板匹配置信度阈值（识别不到可调低） |
| `p2_region_ratio_x/y` | 0.2 / 0.2 | P2 检测区 = 屏幕宽/高 × 比例 |
| `wechat_url_timeout` | 5 | 等待链接超时（扩展模式自动放宽到 ≥15s） |

## 构建

```bash
# 开发构建
cargo build

# 发布构建 + 打包（二进制 / 双扩展 zip / 完整 tar.gz，产物在 dist/）
cargo build --release
nu build.nu all
```

> 需要 `nushell`（`nu`）执行 build.nu；Tailwind CSS 已在 `static/` 预编译，一般无需重新生成。
> 修改模板类名后需重新编译（Tailwind v4，CSS-first 配置见 `static/input.css`）：
> ```bash
> tailwindcss -i static/input.css -o static/tailwind.css --minify
> ```

## 项目结构

- `src/` — Rust 源码（`main.rs` 服务与路由；`config.rs` 配置；`detect.rs` 截图与模板匹配；`mouse.rs` 鼠标控制；`wechat/` 微信劫持子模块）
- `templates/` — 登录页 / 设置页（minijinja 模板）
- `extension/` — 浏览器扩展（Chrome MV3 / Firefox MV2）
- `static/` — 编译后的 CSS 与图标
- `docs/` — 架构文档与设计

## License

[MIT](LICENSE)
