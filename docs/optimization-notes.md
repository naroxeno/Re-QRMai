# Re-QRMai 架构优化建议

> 状态：评审结论 ｜ 基于 2026-08 代码快照（约 1800 行 Rust + 前端/扩展/脚本）

本文档基于对全部源码的评审，按「投入产出比」分级列出架构优化空间。
每项含：现状 → 问题 → 建议 → 工作量。

---

## 一、高优先级（收益大、改动可控，建议尽快做）

### 1.1 拆分 lib.rs，结束「单 main.rs + 无法复用」

**现状**：项目只有 `src/main.rs`（~800 行）+ mouse/wechat/detect 3 个模块，无 `lib.rs`。
**问题**：
- `main.rs` 同时承担配置、路由、QR 处理、日志、静态资源自举、启动编排，单一职责过重；
- **examples 无法引用内部模块**——`examples/visualize_match.rs` 被迫复制了整套梯度匹配逻辑（已实际踩坑，主逻辑每改一次都要同步副本）；
- 核心算法无法写独立单元测试。

**建议**：
- 新建 `src/lib.rs` 暴露 `detect`/`config` 等可测模块，`main.rs` 只留启动编排；
- 按职责拆 `main.rs`：`routes/mod.rs`（路由）、`config.rs`（配置）、`qr.rs`（二维码生成）、`logging.rs`（日志）、`state.rs`（共享状态）。
**工作量**：中（纯搬移，风险低，可逐步迁移）。

### 1.2 配置系统重构：类型安全 + 校验 + 免手写表单映射

**现状**：`Config` 结构体 30+ 字段且持续膨胀；`save_settings` 用 `BTreeMap<String,String>` 逐字段 `match` 手写解析——新增字段必须同步改 4 处（结构体/Default/serde/表单/HTML），极易遗漏。
**问题**：
- 手写映射脆弱（`standalone_mode` 还要特殊处理「表单缺字段=关闭」）；
- 无校验：port 可填 99999、ratio 可填 5.0、坐标可超屏幕范围；
- 大量**预留字段**（`decode.time/retry_count、cache_duration、standalone_mode、dev_mode、version、skin_*`）从未在业务逻辑生效，但占满配置面和设置页。
**建议**：
- 预留字段要么落地要么移除（见 §4.4 技术债）；
- 表单解析改为按字段类型声明式映射（如 `serde_qs` 或手写 `ConfigPatch` 结构体 + `serde` 反序列化表单），消除手写 match；
- 新增 `validate()` 方法：port 1-65535、ratio 0-1、坐标在屏幕范围内（可结合截图尺寸校验）。

### 1.3 模板渲染缓存（当前每次请求重新编译模板）

**现状**：`settings_page` 每次请求都 `Environment::new()` + `add_template`（minijinja 编译）。
**问题**：设置页每次刷新都重新编译模板字符串，纯浪费；且 `Environment` 不可跨请求复用。
**建议**：把 `Environment` 做成启动时初始化一次的共享状态（`manage` 注入），请求只做 `get_template().render()`。

### 1.4 核心逻辑补测试（当前仅 2 个配置测试）

**现状**：全项目只有刚加的配置兼容性测试。
**问题**：detect.rs 的模板匹配（梯度/多尺度/pick 语义）、wechat.rs 的 HTML 正则解析与 URL join、qrmai 的双链路编排——都是最容易回归的代码，且**全部可用合成数据单测**（无需真实屏幕/微信）。
**建议**：
- `detect.rs`：合成图像（固定图案模板 + 已知位置截图）验证多尺度匹配与 P1 锚定 P2 的语义选择，断言坐标误差 < 阈值；
- `wechat.rs`：把 `fetch_and_decode` 的 HTML 提取/URL join 抽成纯函数并单测（配合 `wiremock` 或本地 `tcp` 测试服务）；
- 配合 1.1 的 lib 化后可直接测试。

### 1.5 qrmai_handler 双链路策略化

**现状**：劫持/扩展两条链路的 if/else 交织在 `qrmai_handler`（~100 行），状态（QrCache/HijackState/超时）散落。
**问题**：新增捕获方式（如未来的 bwrap 链路）会继续堆 if/else。
**建议**：抽象 `trait QrCapture`（`async fn fetch_qr(&self) -> Result<String>`），`HijackCapture`/`ExtensionCapture` 各自实现，handler 按 `capture_mode` 选择实现——与 1.1 的 routes 拆分一起做。

## 二、中优先级

### 2.1 错误处理：去掉 20 个 unwrap/expect

**现状**：全项目 20 处 `unwrap()/expect()`，集中在启动路径（`ensure_img_dir`、`init_logger`）和解析处。
**问题**：任何一处失败（磁盘只读、日志目录不可写、模板损坏）直接 panic 崩溃，无恢复路径。
**建议**：启动路径改为优雅降级（如日志初始化失败 → 退化为 stderr-only；模板写出失败 → 继续运行并在设置页提示）；运行期解析统一 `anyhow` 向上传播。

### 2.2 QR 抓取健壮性（HTML 解析是单点脆弱）

**现状**：`fetch_and_decode` 用两个正则从 HTML 提取图片 src（主正则 + fallback 到第一个 `<img>`）。
**问题**：MAID 页面结构一改即失效；`decode.retry_count` 预留了但解码无重试；无页面结构变化时的告警。
**建议**：
- 解析改为宽松的「候选收集」：提取所有 `<img src>`，逐个尝试解码，直到成功（天然覆盖主/备正则，也覆盖页面加壳）；
- 接入 `decode.retry_count`：下载/解码失败重试；
- 打印失败原因分级（404 / 解析失败 / 解码失败）便于排查。

### 2.3 扩展模式：轮询缓存改 channel

**现状**：`GET /qrmai`（扩展模式）200ms 轮询 `QrCache` 直到超时；`POST /url` 写入缓存。
**问题**：轮询有延迟、超时 5s 固定、两请求间靠共享状态隐式耦合。
**建议**：改用 `tokio::sync::oneshot/broadcast` 通道：`POST /url` 向等待的请求投递结果，`GET /qrmai` 等待通道而非轮询；同时把等待超时（`wechat_url_timeout`）作为请求参数下传。

### 2.4 wechat.rs 状态机简化 + 拆分

**现状**：`WechatHijack` 同时持有 `wechat_proc: Option<Child>` 与 `wechat_pid: Option<u32>`（冗余、易不一致），且模块内混着：进程管理、FIFO 监听、URL 抓取、QR 解码、崩溃恢复 5 类职责。
**问题**：bwrap 链路落地后（见 §4.5）会进一步膨胀。
**建议**：按职责拆为 `wechat_proc.rs`（进程/探活/清理）、`qr_fetch.rs`（抓取+解码）、`hijack.rs`（FIFO+伪装）；启动方式抽象为 `enum LaunchMode { Direct, Bwrap }`。

### 2.5 梯度计算缓存

**现状**：`to_gradient_f32` 在 P1/P2 各调用一次，对全屏 2560×1440 做两遍 Sobel。
**问题**：重复计算（一次 /detect_positions 两遍，无大碍但可避免）。
**建议**：`detect_p1p2` 内转一次梯度图，P1/P2 共用（顺便让 §4.1 的 NMS 复用同一梯度图）。

## 三、低优先级 / 远期

### 3.1 安全加固
- `GET /qrmai` 无鉴权即触发鼠标点击：局域网内任何人可触发点击与打开链接。建议支持可选 token（查询参数）或绑定回环地址；
- Rocket secret key 未显式配置：重启后 cookie 失效、签名可预测。建议固定 `secret_key`；
- token 明文存于 config.json 与扩展 `storage.sync`：可接受（本地工具），但文档应说明风险（架构文档已提及）。

### 3.2 平台模块化
detect.rs 内 Windows/macOS 截图分支各 ~50 行 cfg 代码，建议拆为 `capture_linux.rs / capture_windows.rs / capture_macos.rs`（或独立 crate），统一 `capture_screen()` 接口。

### 3.3 CI 与发布
`.github/` 为空。建议恢复 GitHub Actions：cargo fmt/clippy/test + release 构建 + 打三平台发布包（build.nu 的逻辑可平移到 CI）。

### 3.4 模板匹配进阶
- SSD 每尺度只取全局最优（`find_extremes`），可升级为**局部极值 + NMS**，收集同尺度多个候选，配合 P1 锚定提高命中率；
- `template-matching` crate 只有 SSD/SAD，无 NCC——若后续遇到光照/主题变化问题，可评估自实现归一化互相关或换用 `imageproc`。

### 3.5 bwrap 链路落地
`docs/bwrap-hijack-design.md` 已设计完成但未实现；落地时建议与 2.4 的 `LaunchMode` 一起做，避免二次重构。

## 四、技术债清单（顺手清理）

### 4.1 预留字段治理
以下字段仅存在于配置/表单，业务逻辑未使用：`decode.time`、`decode.retry_count`（2.2 可落地）、`cache_duration`（QR 结果缓存可落地）、`standalone_mode`、`dev_mode`、`version`、`skin_*`（皮肤系统未实现）。**要么实现要么从配置/设置页移除**，减少认知负担。

### 4.2 死代码与警告
`MatchResult.confidence` 字段有 dead_code 警告（detect_p1p2 只用 x/y）；`Cargo.toml` 有 unused manifest key（resolver/target.rustflags 位置错误，应移到 `.cargo/config.toml`）。

### 4.3 共享状态收敛
`SharedConfig`/`QrCache`/`HijackState` 三个状态目前各自 `Arc<RwLock>` 传递，建议收敛为单一 `AppState`（内部含三者的 typed 字段），减少 `manage()` 参数噪音。

## 五、建议实施顺序（两周一期的量级）

1. **期 1**：1.1 lib 化 + 1.3 模板缓存 + 4.2 清理（纯重构，无行为变化，风险最低）；
2. **期 2**：1.2 配置系统 + 1.4 核心测试（先测后改，防止重构回归）；
3. **期 3**：2.2 抓取健壮性 + 2.3 channel + 2.4 wechat 拆分；
4. **远期**：3.x 安全/CI/平台模块，按需。

> 原则：优先做「无行为变化的纯重构」（期 1）建立可测性，再动行为；每次改动保持 `cargo test` 全绿。
