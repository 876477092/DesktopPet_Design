#!/usr/bin/env bash
# =============================================================================
# DesktopPet · 零对外连接校验（C9 / `02 §5 K-9`，S6-M3 交付物）
#
# 校验口径（2026-09-13 修正，P1-1 / F-13 闭环，见 03 台账）：tauri 2.11.5
# **传递**依赖含 reqwest/hyper（已登记豁免 `02 §2.5`），照原字面扫 `cargo tree`
# 必然假失败——故按「**直接依赖零网络 + 运行期零外连**」双面断言：
#   1. workspace 各 crate 的 Cargo.toml **直接依赖**不出现网络栈 crate
#      （reqwest / hyper / ureq / tokio-tungstenite / websocket / surf）；
#   2. capabilities/*.json 不开放网络类权限（http/https/fetch/shell 等）；
#   3. tauri.conf.json 的 CSP connect-src 仅允许 `'self'` 与本地 IPC
#      （`ipc:` / `http://ipc.localhost`），不出现外部域名；
#   4. WebView2 additionalBrowserArgs 显式禁用遥测与后台网络
#      （--disable-background-networking / --disable-component-update /
#       --disable-sync / --disable-domain-reliability）；
#   5. 源码不直接调用网络 API（fetch / XMLHttpRequest / reqwest / std::net::TcpStream 白名单外）。
#
# 用法：bash scripts/check-no-net.sh（在仓库根执行）；全部通过 exit 0。
# =============================================================================
set -u

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAIL=0

fail() {
    echo "  [FAIL] $*" >&2
    FAIL=1
}
ok() {
    echo "  [ok] $*"
}

echo "[check-no-net] 1/5 直接依赖扫描（workspace Cargo.toml）"
# 网络栈直接依赖关键词（Cargo.toml 的 dependency 行）。
NET_CRATES='reqwest|hyper|ureq|tokio-tungstenite|websocket|^surf|attohttpc|isahc|minreq'
while IFS= read -r manifest; do
    # 跳过 lock/缓存
    case "$manifest" in
        *node_modules*|*target*) continue ;;
    esac
    # 仅看 [dependencies] 段（不含 dev-dependencies 的间接引用）。
    section=""
    while IFS= read -r line; do
        case "$line" in
            \[dependencies\]*|\[dependencies\.*) section=dep ;;
            \[*) section=other ;;
        esac
        if [ "$section" = "dep" ]; then
            if echo "$line" | grep -Eq "^\s*(name\s*=\s*)?\"?($NET_CRATES)" \
                || echo "$line" | grep -Eq "^\s*($NET_CRATES)\s*[=,]"; then
                fail "Cargo.toml 直接依赖含网络 crate：$manifest → $line"
            fi
        fi
    done < "$manifest"
    ok "扫描 $manifest"
done < <(find "$REPO_ROOT" -name Cargo.toml -not -path '*/target/*' -not -path '*/node_modules/*')

echo "[check-no-net] 2/5 capability 权限面扫描"
CAP_DIR="$REPO_ROOT/src-tauri/capabilities"
if [ -d "$CAP_DIR" ]; then
    for cap in "$CAP_DIR"/*.json; do
        [ -e "$cap" ] || continue
        # 网络 / 高风险权限标识：http(s) 插件、fetch、shell、dialog、fs 写入远程。
        if grep -Eq '"(http|https|fetch|shell|dialog|updater|process|clipboard-manager|global-shortcut):(default|allow|deny)' "$cap"; then
            fail "capability 开放了网络/高风险权限：$cap"
        else
            ok "capability 无网络/高风险权限：$cap"
        fi
    done
else
    ok "无 capabilities 目录（未配置权限面）"
fi

echo "[check-no-net] 3/5 CSP connect-src 校验"
CONF="$REPO_ROOT/src-tauri/tauri.conf.json"
if [ -f "$CONF" ]; then
    # 提取 csp 字段（简化：全文件匹配 connect-src 段）。
    csp=$(grep -o '"csp"[^,]*' "$CONF" | head -1)
    if echo "$csp" | grep -q "connect-src 'self' ipc: http://ipc.localhost"; then
        ok "CSP connect-src 仅本地（'self' + ipc.localhost）"
    else
        fail "CSP connect-src 非预期：$csp"
    fi
    # 禁止任何外部域名出现在 connect-src（http(s):// 且非 ipc.localhost）。
    # （grep 基础正则无环视，改用逐 token 校验。）
    conn=$(echo "$csp" | grep -oE 'connect-src[^;]*' | head -1)
    bad=""
    for token in $conn; do
        case "$token" in
            connect-src|'self'|ipc:|http://ipc.localhost) ;;
            *) bad="$bad $token" ;;
        esac
    done
    if [ -n "$bad" ]; then
        fail "CSP connect-src 含外部源：$bad"
    else
        ok "CSP connect-src 无外部域名"
    fi
else
    fail "缺少 tauri.conf.json：$CONF"
fi

echo "[check-no-net] 4/5 WebView2 遥测禁用断言"
if [ -f "$CONF" ]; then
    args=$(grep -o '"additionalBrowserArgs"[^,}]*' "$CONF" | head -1)
    for flag in --disable-background-networking --disable-component-update --disable-sync --disable-domain-reliability; do
        if echo "$args" | grep -q -- "$flag"; then
            ok "additionalBrowserArgs 含 $flag"
        else
            fail "additionalBrowserArgs 缺少 $flag"
        fi
    done
fi

echo "[check-no-net] 5/5 源码网络调用扫描（白名单外）"
# 白名单：ipc 协议本地调用、asset 本地加载、CSP 内的 localhost（dev 热更）。
if grep -rnE 'fetch\(|XMLHttpRequest|WebSocket\(|new Request\(' "$REPO_ROOT/src" --include='*.ts' --include='*.tsx' 2>/dev/null \
    | grep -vE 'http://localhost:5173|ipc\.localhost|ws://localhost:5173' | grep -q .; then
    fail "前端源码存在对外网络调用（见上）"
else
    ok "前端源码无对外网络调用"
fi

# Rust 侧：禁止直接网络栈（std::net 的 TcpStream/UdpSocket 亦属网络面；本地 IPC 走 tauri 事件不在此列）。
if grep -rnE 'std::net::|reqwest::|hyper::|ureq::' "$REPO_ROOT/src-tauri/crates" --include='*.rs' 2>/dev/null | grep -v '/tests/' | grep -q .; then
    fail "Rust 源码存在直接网络 API 调用（见上）"
else
    ok "Rust 源码无直接网络 API 调用"
fi

echo ""
if [ "$FAIL" -eq 0 ]; then
    echo "[check-no-net] 全部通过：直接依赖零网络 + 运行期零外连（C9）"
    exit 0
else
    echo "[check-no-net] 存在失败项（见上）" >&2
    exit 1
fi
