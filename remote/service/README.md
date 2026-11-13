# 手机访问：手机网关与命名隧道

手机用浏览器打开一个由你自己的 Mac 提供的网页，就能浏览和整理这台 Mac 上的资料库。资料始终留在
Mac 上：手机和 Mac 之间端到端加密，公网上只有你自己的 Cloudflare 命名隧道，它把一个域名转发到
Mac 本机的手机网关，转发的内容它读不到。

整条链路由三部分组成，都运行在你的 Mac 上：

1. **memex 守护进程**：管理资料库，只通过属主私有的本地 socket 对外服务。
2. **手机网关** `remote/service/mobile-gateway.js`：一个只监听 `127.0.0.1` 的 Node 进程，提供手机网页
   `mobile/web`，负责加密中继，并以受限权限连接守护进程的 socket。
3. **cloudflared 命名隧道**：把 `https://<你的域名>/` 转发到手机网关的本机端口，此外的一切请求都返回 404。

## 前置条件

- macOS，已从本仓库构建 `memex`：`cargo build --release -p memex-cli`，产物在 `target/release/memex`。
- Node.js 24，在仓库根目录执行过 `npm ci`，以安装 `ws`、`qrcode` 和 `jsqr`。
- [cloudflared](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/)，以及一个托管在
  Cloudflare 的域名。下文用 `phone.example.com` 代表你的手机访问域名，请换成自己的。

## 第 1 步：启动 memex 守护进程

```sh
target/release/memex daemon install
target/release/memex daemon status
```

`install` 会写入默认配置 `~/Library/Application Support/LotsMemeX/config.json`，并注册 LaunchAgent
`com.lotsmemex.agent`。守护进程的 socket 位于 `~/Library/Application Support/LotsMemeX/agent.sock`，
权限为 `0600`。

## 第 2 步：准备手机网关的私有目录

```sh
install -d -m 700 "$HOME/Library/Application Support/LotsMemeX/mobile-gateway"
```

网关把配对状态、准入根密钥和临时二维码保存在这个目录里，文件权限都是 `0600`。目录必须归当前用户
所有、权限为 `0700`，且不能是符号链接，否则网关拒绝启动。不要把这个目录提交到任何仓库，也不要
复制到别的机器。

## 第 3 步：手机网关的环境变量

| 变量 | 必填 | 含义 |
|---|---|---|
| `MEMEX_AGENT_SOCKET` | 是 | 守护进程 socket 的绝对路径，即第 1 步中的 `agent.sock` |
| `MEMEX_MOBILE_PUBLIC_URL` | 是 | 手机访问的 HTTPS 源，只写到域名并以 `/` 结尾，例如 `https://phone.example.com/` |
| `MEMEX_MOBILE_ROOT` | 是 | 本仓库 `mobile/web` 目录的绝对路径 |
| `MEMEX_MOBILE_STATE_DIR` | 是 | 第 2 步创建的私有目录 |
| `MEMEX_MOBILE_PORT` | 是 | 网关监听的本机端口，取 1024 到 65535 之间，例如 `48731` |
| `MEMEX_MOBILE_MAX_SENSITIVITY` | 否 | 手机能读取的最高密级：`public`、`internal` 或 `private`，默认 `private` |
| `MEMEX_MOBILE_GATEWAY_LABEL` | 否 | 网关的 LaunchAgent 标签，`pair` 命令用它重启网关，默认 `com.lotsmemex.mobile-gateway` |
| `MEMEX_MOBILE_BRAND_NAME` / `MEMEX_MOBILE_ASSISTANT_NAME` | 否 | 手机界面显示的产品名和助手名，默认 `Lots MemeX` / `MemeX` |

网关只监听 `127.0.0.1`，绝不在局域网或公网上直接开放端口。

## 第 4 步：把手机网关注册为 LaunchAgent

把下面的内容保存为 `~/Library/LaunchAgents/com.lotsmemex.mobile-gateway.plist`，并替换其中的路径、
用户名、域名和端口。`node` 请写绝对路径，可以用 `command -v node` 查到。

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.lotsmemex.mobile-gateway</string>
  <key>ProgramArguments</key><array>
    <string>/opt/homebrew/bin/node</string>
    <string>/Users/you/lotsmemex/remote/service/mobile-gateway.js</string>
    <string>run</string>
  </array>
  <key>EnvironmentVariables</key><dict>
    <key>MEMEX_AGENT_SOCKET</key><string>/Users/you/Library/Application Support/LotsMemeX/agent.sock</string>
    <key>MEMEX_MOBILE_PUBLIC_URL</key><string>https://phone.example.com/</string>
    <key>MEMEX_MOBILE_ROOT</key><string>/Users/you/lotsmemex/mobile/web</string>
    <key>MEMEX_MOBILE_STATE_DIR</key><string>/Users/you/Library/Application Support/LotsMemeX/mobile-gateway</string>
    <key>MEMEX_MOBILE_PORT</key><string>48731</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/dev/null</string>
  <key>StandardErrorPath</key><string>/dev/null</string>
</dict></plist>
```

```sh
chmod 600 ~/Library/LaunchAgents/com.lotsmemex.mobile-gateway.plist
plutil -lint ~/Library/LaunchAgents/com.lotsmemex.mobile-gateway.plist
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/com.lotsmemex.mobile-gateway.plist
launchctl print "gui/$(id -u)/com.lotsmemex.mobile-gateway" | grep -E 'state|pid'
```

网关在 macOS 的图形会话里运行，因为第一次启动时要在 Mac 屏幕上显示配对二维码。日志只记录固定的事件名，
不包含资料内容、配对码或密钥。

## 第 5 步：建立 cloudflared 命名隧道

```sh
cloudflared tunnel login
cloudflared tunnel create memex-phone
cloudflared tunnel route dns memex-phone phone.example.com
```

`create` 会打印隧道 UUID，并把凭据写到 `~/.cloudflared/<UUID>.json`。新建配置文件
`~/.cloudflared/memex-phone.yml`，内容如下，ingress 只放这一个域名，最后一条兜底返回 404：

```yaml
tunnel: <UUID>
credentials-file: /Users/you/.cloudflared/<UUID>.json
ingress:
  - hostname: phone.example.com
    service: http://127.0.0.1:48731
  - service: http_status:404
```

`service` 里的端口必须与 `MEMEX_MOBILE_PORT` 一致。不要在这份配置里加其他 hostname 或路径规则，
也不要改成转发守护进程的 socket。先在前台试运行：

```sh
chmod 600 ~/.cloudflared/memex-phone.yml ~/.cloudflared/<UUID>.json
cloudflared tunnel --config ~/.cloudflared/memex-phone.yml run memex-phone
```

确认可用后，可以照第 4 步的写法，为
`cloudflared tunnel --no-autoupdate --config <配置文件> run memex-phone` 再写一个 LaunchAgent，
让它常驻运行。

## 第 6 步：配对手机

1. 网关第一次启动时没有已信任的手机，会在 Mac 屏幕上用快速查看窗口显示一张配对二维码，有效期 5 分钟。
2. 在手机浏览器（iOS 需要 18.4 或更新版本的 Safari）打开 `https://phone.example.com/mobile/index.html`，
   点 **扫描配对二维码**，扫描 Mac 上的二维码。二维码不是网址，只在这个页面里解码和校验。
3. 配对成功后二维码窗口自动关闭，手机记住这台 Mac。可以把页面添加到主屏幕。

二维码过期、换手机，或者要撤销已配对的手机时，用和网关相同的环境变量运行：

```sh
node remote/service/mobile-gateway.js pair              # 撤销现有手机，重启网关并显示新二维码（5 分钟）
node remote/service/mobile-gateway.js pair --minutes=30 # 仅这一次放宽到 30 分钟
```

`pair` 只重启 `MEMEX_MOBILE_GATEWAY_LABEL` 指定的那一个服务，不会影响其他进程。

## 安全说明

- 手机和 Mac 之间的内容端到端加密，网关的中继部分和 Cloudflare 都只能看到密文。
- 手机只拿到读取、编辑和采集权限，读取范围不超过 `MEMEX_MOBILE_MAX_SENSITIVITY`，拿不到守护进程的其他能力。
- 不要把 `MEMEX_MOBILE_STATE_DIR` 里的文件、隧道凭据或二维码截图发给任何人。
- 停用手机访问：先 `launchctl bootout "gui/$(id -u)/com.lotsmemex.mobile-gateway"`，再停掉隧道；资料库不受影响。
- 更换手机访问的域名后，手机端的身份、信任和待发记录都不会迁移到新域名：新域名从空白状态开始，需要重新配对。

## 进阶：中继、桥接、隧道分开运行

`run-service.sh`、`preflight.sh`、`service-control.sh` 和 `render-launchagents.cjs` 是另一种部署方式：
网页由别处托管，Mac 上分别运行三个 LaunchAgent，即中继 `com.lotsmemex.remote-relay`、桥接
`com.lotsmemex.remote-bridge` 和隧道 `com.lotsmemex.remote-tunnel`。配置写在
`~/Library/Application Support/LotsMemeX/remote-managed/remote-service.env` 里，以
`remote-service.env.template` 为模板，文件权限为 `0600`。网页源、网页客户端地址和中继域名分别由
`MEMEX_REMOTE_BROWSER_ORIGIN`、`MEMEX_REMOTE_WEB_CLIENT_URL` 和 `MEMEX_REMOTE_RELAY_HOSTNAME`
指定。启动前 `preflight.sh` 会核对文件权限、仓库提交和隧道 hostname；`service-control.sh` 只按精确标签
启停这三个服务，从不按进程名结束进程。只是想用手机访问的话，用上面的手机网关就够了。
