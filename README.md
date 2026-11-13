# Lots MemeX

Lots MemeX 是一个本地优先的个人资料库：网页、文章、书和你的批注都保存在自己的 Mac 上，用 Mac 客户端阅读和整理；
需要时也可以用手机通过端到端加密连回这台 Mac。没有云端账号，也不需要把资料交给第三方。

网页采集和正文提取由姊妹项目 [lots-scout](https://github.com/lotsmemex/lots-scout) 提供，其中包括
Chrome 扩展和提取器。

## 下载 Mac 客户端

从 [Releases](https://github.com/lotsmemex/lotsmemex/releases/latest) 下载 `LotsMemeX.dmg`，打开后把
`LotsMemeX.app` 拖进「应用程序」。目前只支持 Apple 芯片的 Mac。

第一版没有经过 Apple 公证，第一次打开时 macOS 会拦截。请到「系统设置 → 隐私与安全性」，在页面下方
找到 LotsMemeX 并点「仍要打开」。也可以在终端执行：

```sh
xattr -dr com.apple.quarantine /Applications/LotsMemeX.app
```

Mac 客户端读取本机的 memex 守护进程，所以第一次使用前请按下文「从源码构建」安装并启动守护进程。

## 仓库内容

| 目录 | 内容 |
|---|---|
| `crates/memex-*` | 资料库、存储、检索、采集队列和守护进程，命令行入口为 `memex` |
| `desktop/gpui` | Mac 客户端（GPUI），二进制为 `memex-desktop` |
| `mobile/web` | 手机网页客户端 |
| `remote` | 手机与 Mac 之间的加密中继、桥接和手机网关 |
| `interfaces`、`capabilities` | 接口契约和能力说明 |

## 从源码构建

需要 macOS、Rust（版本见 `rust-toolchain.toml` 及各 crate 的配置）、Node.js 24。

```sh
cargo build --release -p memex-cli          # 产物：target/release/memex
target/release/memex daemon install         # 注册并启动本机守护进程
npm ci                                      # 手机网关所需的 Node 依赖
desktop/gpui/scripts/package-dev-app.sh     # 构建 Mac 客户端（ad-hoc 签名，产物在 desktop/gpui/target/app）
```

测试：

```sh
cargo test --workspace
(cd desktop/gpui && cargo test --workspace)
npm test
```

## 用手机访问

照 [`remote/service/README.md`](remote/service/README.md) 配置手机网关和 Cloudflare 命名隧道，然后用手机
扫码配对。

## 许可

Apache License 2.0，见 [LICENSE](LICENSE) 和 [NOTICE](NOTICE)。
