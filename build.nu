#!/usr/bin/env nu
# ── QRMai-rs 编译 & 打包脚本 (Nushell) ──────────────────────
#
# 用法:
#   nu build.nu              # 开发构建（debug）
#   nu build.nu release      # 发布构建（release）
#   nu build.nu extension    # 仅打包浏览器扩展
#   nu build.nu all          # 构建 + 打包所有产物
#
# 输出:
#   dist/QRMai-rs                   # 可执行文件
#   dist/extension-chrome.zip       # Chrome 扩展包
#   dist/extension-firefox.zip      # Firefox 扩展包
#   dist/qrmai-rs-{version}.tar.gz  # 完整发布包

# ── 版本号 ────────────────────────────────────────────

let version = (open Cargo.toml | get package.version)
let dist_dir = "dist"
let ext_dir = "extension"

# ── 工具函数 ──────────────────────────────────────────

def info [msg: string] {
    print $"(ansi cyan)[BUILD](ansi reset) ($msg)"
}

def ok [msg: string] {
    print $"(ansi green)[  OK](ansi reset) ($msg)"
}

def err [msg: string] {
    print $"(ansi red)[FAIL](ansi reset) ($msg)"
}

# ── 清理 ──────────────────────────────────────────────

def clean [] {
    info "清理旧的构建产物..."
    rm -rf $dist_dir
    mkdir $dist_dir
}

# ── 构建 QRMai 本体 ───────────────────────────────────

def build-binary [profile: string = "debug"] {
    let msg = "编译 QRMai-rs v" + $version + " (" + $profile + ")"
    info $msg

    if $profile == "release" {
        cargo build --release
    } else {
        cargo build
    }

    let bin = if $profile == "release" {
        "target/release/QRMai-rs"
    } else {
        "target/debug/QRMai-rs"
    }

    if not ($bin | path exists) {
        err $"编译产物未找到: ($bin)"
        exit 1
    }

    cp $bin $"($dist_dir)/QRMai-rs"
    chmod +x $"($dist_dir)/QRMai-rs"
    ok $"二进制文件 → ($dist_dir)/QRMai-rs"
}

# ── 打包浏览器扩展 ─────────────────────────────────────

def pack-extension [] {
    info "打包浏览器扩展..."

    let tmp = (mktemp -d)
    let cwd = $env.PWD

    # ── Chrome 版本 (Manifest V3) ──
    let chrome_dir = $"($tmp)/chrome"
    mkdir $chrome_dir

    [
        "manifest.json",
        "background.js",
        "options.html",
        "options.js",
    ] | each {|f| cp $"($ext_dir)/($f)" $chrome_dir }

    if ("icon.png" | path join $ext_dir | path exists) {
        cp $"($ext_dir)/icon.png" $chrome_dir
    }

    cd $chrome_dir
    ^zip -qr $"($cwd)/($dist_dir)/extension-chrome.zip" ...(ls | get name)
    cd $cwd
    ok $"Chrome 扩展 → ($dist_dir)/extension-chrome.zip"

    # ── Firefox 版本 (Manifest V2) ──
    let ff_dir = $"($tmp)/firefox"
    mkdir $ff_dir

    cp $"($ext_dir)/manifest.firefox.json" $"($ff_dir)/manifest.json"

    [
        "background.js",
        "options.html",
        "options.js",
    ] | each {|f| cp $"($ext_dir)/($f)" $ff_dir }

    if ("icon.png" | path join $ext_dir | path exists) {
        cp $"($ext_dir)/icon.png" $ff_dir
    }

    cd $ff_dir
    ^zip -qr $"($cwd)/($dist_dir)/extension-firefox.zip" ...(ls | get name)
    cd $cwd
    ok $"Firefox 扩展 → ($dist_dir)/extension-firefox.zip"

    rm -rf $tmp
}

# ── 打包完整发布包 ─────────────────────────────────────

def pack-release [profile: string = "debug"] {
    let arch = (^uname -m | str trim)
    let os = (^uname -s | str trim)
    let cwd = $env.PWD
    let pkg_name = $"qrmai-rs-v($version)-($os)-($arch)"

    info $"打包完整发布: ($pkg_name)"

    let pkg_dir = $"($dist_dir)/($pkg_name)"
    mkdir $pkg_dir

    cp $"($dist_dir)/QRMai-rs" $pkg_dir
    cp $"($dist_dir)/extension-chrome.zip" $pkg_dir
    cp $"($dist_dir)/extension-firefox.zip" $pkg_dir

    if ("config.json" | path exists) { cp "config.json" $pkg_dir }
    if ("README.md" | path exists)   { cp "README.md" $pkg_dir }
    if ("img" | path exists)         { cp -r "img" $"($pkg_dir)/img" }

    cd $dist_dir
    tar czf $"($pkg_name).tar.gz" $pkg_name
    cd $cwd
    rm -rf $pkg_dir

    ok $"发布包 → ($dist_dir)/($pkg_name).tar.gz"
}

# ── 主入口 ──────────────────────────────────────────────

def main [mode: string = "debug"] {
    match $mode {
        "release" => {
            clean
            build-binary "release"
            pack-extension
            pack-release "release"
        }
        "all" => {
            clean
            build-binary "release"
            pack-extension
            pack-release "release"
        }
        "extension" => {
            clean
            pack-extension
        }
        _ => {
            clean
            build-binary "debug"
            pack-extension
        }
    }

    info $"完成！产物在 ($dist_dir)/"
    ^ls -lh $dist_dir
}
