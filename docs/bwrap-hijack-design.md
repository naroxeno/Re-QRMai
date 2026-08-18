# bwrap（bubblewrap）沙箱微信的 QR 劫持方案设计

> 状态：设计稿 ｜ 关联：src/wechat.rs ｜ 平台：Linux

本文档解决一个具体问题：**当微信是通过 bwrap（bubblewrap）包裹脚本启动时
（例如 AUR 的 `wechat-universal-bwrap`、部分发行版商店版微信），
现有「PATH 前置伪装 xdg-open」的劫持方案失效**，需要改用 bwrap 原生能力实现劫持。

---

## 1. 背景：bwrap 包裹的微信长什么样

bwrap 包裹脚本本质上是：

```bash
#!/bin/bash
exec bwrap \
  --unshare-all \
  --die-with-parent \
  --ro-bind /usr /usr \
  --ro-bind /opt/wechat /opt/wechat \
  --bind "$XDG_RUNTIME_DIR" "$XDG_RUNTIME_DIR" \
  ... \
  /opt/wechat/wechat "$@"
```

关键事实：
- 微信运行在**新的 mount namespace** 中，看到的文件系统是沙箱视图；
- 沙箱内 `/usr/bin/xdg-open` 通常是宿主机真实文件被只读 bind 进来的；
- 沙箱的 PATH / 环境变量由包裹脚本决定（可能 `--clearenv` 或重设 PATH）；
- 微信在沙箱内 fork/exec `xdg-open`（可能 PATH 查找，也可能绝对路径）。

## 2. 现有方案为什么失效

| 现有劫持手段 | 在 bwrap 沙箱下的表现 |
|---|---|
| 生成伪装 `xdg-open` 并 `PATH=$FAKE:$PATH` 启动微信 | 沙箱内 PATH 可能被重置；微信可用绝对路径调用；即使命中 PATH，沙箱内也找不到 `$FAKE` 目录（不在沙箱视图） |
| 伪装脚本把 URL 写入宿主 FIFO 绝对路径 | 沙箱内看不到宿主机临时目录；若包裹脚本 `--tmpfs /tmp`，沙箱内 /tmp 是空的 |
| 伪装脚本 `exec /usr/bin/xdg-open "$@"` 转发 | 沙箱内该路径已被自己覆盖 → **无限递归** |

## 3. 核心思路：用 bwrap 的 bind-mount 能力「从外部注入劫持」

bwrap 原生提供：

```
bwrap --ro-bind-try SRC DEST   # 把宿主机 SRC 只读挂载到沙箱内 DEST（SRC 不存在则忽略）
bwrap --bind-try SRC DEST      # 同上，读写
```

利用这一点，**在 bwrap 创建沙箱的瞬间，把宿主机上的伪装文件覆盖进沙箱内对应路径**。
沙箱内微信无论如何调用 xdg-open，命中的都是我们挂载进去的伪装脚本——与 PATH、环境变量、
绝对/相对路径全部无关。这就是「用 bwrap 原生功能实现劫持」。

## 4. 总体架构

```
宿主机                                                   微信沙箱 (bwrap mount ns)
+--------------------------+       注入的挂载组            +-----------------------------+
| QRMai-rs 服务             |                             |                             |
|  +- .fake_bin/           |  --ro-bind-try FAKE_XDG     |  /usr/bin/xdg-open ---> 伪装  |
|  |   +- bwrap (包装器)    | -------------->             |  /usr/local/bin/xdg-open ->伪装|
|  |   +- xdg-open (沙箱版) |                             |  /usr/bin/xdg-open.real <- 真实|
|  +- .link_pipe (FIFO)    |  --bind-try FIFO            |  /tmp/qrmai_pipe <- 同一 inode|
|  +- FIFO 监听线程 --------+-- 读取（同一文件）-----------> 伪装脚本写入 URL                |
+--------------------------+                             +-----------------------------+
```

要点：
- **FIFO 用 bind 挂载进沙箱**：沙箱内写入 `/tmp/qrmai_pipe` = 宿主机 FIFO 写入（同一 inode），
  现有监听线程**零改动**继续工作；
- **真实 xdg-open 备份到 `.real`**：伪装脚本对非 MAID 链接执行 `xdg-open.real`，
  避免自递归，不影响微信正常打开网页。

## 5. 详细设计

### 5.1 产物清单（QRMai-rs 启动时生成，位于临时目录 `$TMP/qrmai_<pid>/`）

| 文件 | 说明 |
|---|---|
| `.fake_bin/bwrap` | 伪装 bwrap 包装器（可执行脚本），注入挂载参数后 exec 真实 bwrap |
| `.fake_bin/xdg-open` | 沙箱版伪装 xdg-open：MAID 链接 → 写 FIFO；其他 → 转发 `xdg-open.real` |
| `.link_pipe` | FIFO（复用现有），宿主监听线程读取 |

### 5.2 注入的挂载组（伪装 bwrap 向真实 bwrap 追加的参数）

```
--ro-bind-try "$FAKE_XDG" /usr/bin/xdg-open
--ro-bind-try "$FAKE_XDG" /usr/local/bin/xdg-open
--ro-bind-try "$FAKE_XDG" /bin/xdg-open
--ro-bind-try /usr/bin/xdg-open /usr/bin/xdg-open.real
--ro-bind-try /usr/local/bin/xdg-open /usr/local/bin/xdg-open.real
--bind-try "$FIFO" /tmp/qrmai_pipe
```

说明：
- 前 3 条覆盖沙箱内可能的 xdg-open 位置（`--ro-bind-try` 对不存在的路径自动跳过，无副作用）；
- 中间 2 条把宿主机真实 xdg-open 备份到 `.real`（供非 MAID 链接转发）；
- 最后 1 条把宿主机 FIFO 以读写方式挂进沙箱固定路径 `/tmp/qrmai_pipe`，
  **不依赖沙箱是否共享 /tmp / XDG_RUNTIME_DIR / 环境变量**。

### 5.3 伪装 bwrap 包装器（`.fake_bin/bwrap`）

```bash
#!/bin/bash
# QRMai-rs 生成的 bwrap 包装器：注入 QR 劫持挂载后转交真实 bwrap
REAL_BWRAP="/usr/sbin/bwrap"          # 生成时写死（which bwrap 探测结果，防自递归）
FAKE_XDG="<TMP>/.fake_bin/xdg-open"
FIFO="<TMP>/.link_pipe"

exec "$REAL_BWRAP" \
    --ro-bind-try "$FAKE_XDG" /usr/bin/xdg-open \
    --ro-bind-try "$FAKE_XDG" /usr/local/bin/xdg-open \
    --ro-bind-try "$FAKE_XDG" /bin/xdg-open \
    --ro-bind-try /usr/bin/xdg-open /usr/bin/xdg-open.real \
    --ro-bind-try /usr/local/bin/xdg-open /usr/local/bin/xdg-open.real \
    --bind-try "$FIFO" /tmp/qrmai_pipe \
    "$@"
```

注意：
- **REAL_BWRAP 必须写死绝对路径**（`which bwrap` 探测后在生成时内嵌），
  因为伪装 bwrap 在 PATH 里，`command -v bwrap` 会找到它自己造成递归；
- bwrap 的所有选项必须在被运行命令之前，注入参数放在 `"$@"` 之前是安全的；
- 注入对微信包裹脚本原有挂载零破坏（追加选项，不改动原参数）。

### 5.4 沙箱版伪装 xdg-open（`.fake_bin/xdg-open`）

```bash
#!/bin/bash
URL="$1"
if [[ "$URL" =~ ^https?://wq\.wahlap\.net/qrcode/req/MAID[0-9A-Fa-f]+\.html ]]; then
    # 写入经 bwrap bind 进沙箱的 FIFO（同一 inode，宿主监听线程直接收到）
    echo "$URL" > /tmp/qrmai_pipe
    exit 0
fi
# 非 MAID 链接：转发到沙箱内的真实 xdg-open（.real 备份）
exec /usr/bin/xdg-open.real "$@" 2>/dev/null \
  || exec /usr/local/bin/xdg-open.real "$@" 2>/dev/null \
  || exec /bin/xdg-open.real "$@"
```

与现有宿主机版脚本的差异：写 FIFO 路径改为沙箱内固定路径 `/tmp/qrmai_pipe`，
转发目标改为 `xdg-open.real`（宿主机版是 `unset BROWSER; exec /usr/bin/xdg-open`）。

### 5.5 让微信包裹脚本命中伪装 bwrap（三种介入方式）

| 方式 | 适用场景 | 做法 |
|---|---|---|
| **A. PATH 前置（推荐）** | 包裹脚本以 `exec bwrap`（无路径）调用 | 复用现有 `PATH=$FAKE:$PATH` 技巧启动包裹脚本 |
| **B. 绝对路径替换** | 包裹脚本以 `exec /usr/bin/bwrap` 调用 | QRMai 生成新包裹脚本（sed 将 `/usr/bin/bwrap` 替换为 `$FAKE/bwrap`），或提示用户改一行 |
| **C. 直接接管 bwrap 参数** | 用户愿意提供包裹脚本的挂载参数 | 配置新增 `wechat_bwrap_args`（JSON 数组），QRMai 自行组装 `bwrap <注入挂载> <用户参数> <wechat_bin>`，完全可控 |

## 6. 与现有代码的集成点（src/wechat.rs）

1. **启动方式探测**：`WechatHijack::init` 读 `wechat_bin` 文件头，若为脚本且含 `bwrap`
   （或配置 `wechat_launch_mode="bwrap-wrapper"`），走新链路；
2. **新增 `create_fake_bwrap()`**：生成 `.fake_bin/bwrap`、沙箱版 `.fake_bin/xdg-open`（5.3/5.4）；
   复用 `create_fake_xdg_open` 的权限设置逻辑；
3. **`launch_wechat` 分路**：bwrap 链路用 `Command::new("sh").arg(包裹脚本)`
   + `env("PATH", fake_dir:orig_path)`（方式 A）；方式 B/C 分别处理；
4. **FIFO 监听零改动**：监听宿主 `.link_pipe`，与沙箱内 `/tmp/qrmai_pipe` 同一 inode；
5. **崩溃恢复**：`HijackState` 增加 `fake_bwrap` 路径字段，恢复时重建监听与伪造文件；
6. **清理**：`cleanup` 删除 `.fake_bin/bwrap`（沙箱随微信进程退出自动销毁，无需 kill bwrap）。

## 7. 新增配置项

```json
{
  "wechat_launch_mode": "auto",        // auto | direct | bwrap-wrapper
  "bwrap_path": "/usr/sbin/bwrap",     // 真实 bwrap 绝对路径（生成时探测并回写）
  "wechat_bwrap_args": []              // 方式 C：用户提供的 bwrap 挂载参数（JSON 数组）
}
```

`auto`：wechat_bin 为脚本且含 `bwrap` → wrapper 链路；否则走现有 direct 链路。

## 8. 时序（方式 A，一次完整请求）

```
浏览器        Rocket                   微信包裹脚本                      微信沙箱
  | GET /qrmai |                         |                                |
  +------------>| PATH=FAKE:$PATH 启动    |                                |
  |             +------------------------>| exec bwrap ...                |
  |             |                         +-> .fake_bin/bwrap 命中（注入挂载）|
  |             |                         |    exec /usr/sbin/bwrap 注入.. |
  |             |                         |    -- 创建沙箱（覆盖 xdg-open）-->|
  |             |  点击 P1 -> P2          |                                | 点击生成二维码
  |             +--------------------------------------------------------->|
  |             |  微信打开 MAID 链接                                    | exec xdg-open
  |             |                                                        |  +-> 伪装脚本命中
  |             |                                                        |  +-> echo URL > /tmp/qrmai_pipe
  |             | <----------- FIFO 收到 URL（bind 同一 inode）------------+
  |             | fetch_and_decode -> PNG
  | <-- PNG ----+
```

## 9. 风险与边界

- **包裹脚本绝对路径调 bwrap**：方式 A 失效，需方式 B（脚本替换）或 C（配置接管）；
- **沙箱内 /usr 不完整**：若包裹脚本未 bind 整个 /usr，`.real` 挂载的 DEST 父目录可能不存在；
  可用 `--bind-try` + 生成时探测规避，或退化为「不转发、直接忽略非 MAID 链接」（影响极小）；
- **安全**：伪装 bwrap 只影响微信自身沙箱的挂载，不修改系统文件；`--ro-bind-try` 保证
  宿主机文件只读；token/路径泄露面与现有方案一致；
- **bwrap 权限**：需要 user namespaces 或 setuid bwrap——包裹脚本能跑说明系统已满足；
- **`--unshare-net` 沙箱**：不影响本方案（URL 回传走 FIFO，不依赖网络）。

## 10. 验证方法

1. `bwrap --ro-bind-try <fake> /usr/bin/xdg-open -- /bin/sh -c 'cat /usr/bin/xdg-open'`
   应输出伪装脚本内容；
2. 沙箱内手动执行伪装脚本传 MAID URL，宿主 `cat .link_pipe` 应能读到；
3. 微信沙箱内执行 `xdg-open https://example.com`，应正常打开浏览器（.real 转发）；
4. 端到端：`curl localhost:5000/qrmai` 返回 PNG。

## 11. 备选方案对比

| 方案 | 侵入性 | 可靠性 | 说明 |
|---|---|---|---|
| **伪装 bwrap + bind 注入（本文）** | 低（不碰系统文件） | 高（与 PATH/环境变量解耦） | 推荐 |
| 修改包裹脚本加 `--ro-bind` | 中（要用户改脚本） | 高 | 一次性手动方案，可作 fallback 文档 |
| 劫持包裹脚本入口（sed 替换 bwrap 路径） | 低 | 中（依赖脚本格式） | 即方式 B |
| 完全由 QRMai 组装 bwrap 命令 | 低 | 高（需用户提供参数） | 即方式 C，最可控 |
| 仅靠 ~/.local/bin 前置 xdg-open | 无 | 低（沙箱 PATH 不可控） | 不推荐 |
