# memex.mobile_library_surface

## Summary

`memex serve --library <root> --max-sensitivity <ceiling> [--port <port>] [--orb-portal]` 在既有
IPv4 loopback local surface 上增加 Mobile Library V0。显式的
`--lan-address <private-ipv4> --lan-hostname <bonjour.local>` 模式在该 private interface 上提供同源
direct capture；它不是公网 API、同步协议或 relay。

## 手机网页版部署授权解释（2026-11-02）

本节适用于现有 `remote/service/mobile-gateway.js` 组装的 relay/bridge 手机网页版，
不把下文历史 local-surface V0 的 GET-only/LAN 规则冒充这个新端的完整权限合同。
本次仅核源码和有界会话依据，不读取私库、凭据、配对材料，不探公网或操作服务。

### 已覆盖的用途与操作

所有者需求
（2026-11-02T10:59:48.389Z）：“接下来优先先帮我把手机端网页版部署实现了”；
#1971（2026-11-02T11:19:37.307Z）：“现在部署的 MemeX 正式网址是什么？
我要打开页面才能扫码”。结合此前真实 MemeX 库接新端、手机粘贴抓取/译文阅读与
标题/标签编辑的目标，此轮覆盖该用户自己的手机通过正式入口配对、访问其 MemeXDB，
以及这些功能所需的 read + edit + capture。无需把整个手机部署或已经要求的编辑/抓取
重新请批；旧 public/只读/1h 是历史建议或合成 fixture 参数，不是现行限制。

- `read`：授权范围内的检索、原文/上下文和已验证译文读取；当前部署 ceiling 为
  private，包含 public/internal/private，拒绝 restricted。不是只有 public 的演示库，
  也不授权匿名访客、其他人的手机或任意公网客户端读库。
- `edit`：当前 scoped router 的标题/manual_tags 编辑，仍需 expected_revision_id
  和 Store 权限/CAS 校验；不是正文、敏感级别、凭据或任意管理操作的写权限。
- `capture`：手机显式粘贴的 URL 经现有准入/抓取/幂等链路入库并查询本配对的任务；
  新条目的既有翻译流程沿原用途授权。不是后台整库抓取/模型推理、取浏览器登录态
  或绕过站点认证。翻译读取接口自身不授予任意模型调用。
- 条目 AI 对话：#2051（2026-11-02T11:39:39.626Z）明确“手机端核心承担的任务，
  是读取、采集和AI对话。Mac电脑端作为一个远端runner始终在线，手机端是随时随地
  可以增、查、聊的场所。”因此显式的所选条目对话属于当前目标，不再列用途待批；
  具体数据流与接口见下节。不是后台整库推理或任意工具执行权限。

三种 scope 只在已认证的配对 session/generation 下生效，调用方不能自增 scope/ceiling。
配对证明设备身份，不自动赋予无限权限或决定有效期限。CLIProxy 的“权限全给”不适用于
手机数据访问授权。正式 URL/HTTPS 也不等于公开数据授权或完整应用 E2EE 证明。

### 365 天已选定；仅重启续期待明确

真人 #2028（2026-11-02T11:36:42.107Z，即北京时间 19:36:42.107）：
“手机端需要全部权限，维持365天权限。现在还没能扫码连接上，优先处理连接问题。”
并说明上架 iOS 前先用 Safari 保存为桌面 App。**365 天期限已经选定**；“全部权限”
结合此轮手机用途指已述读、编辑、采集与显式 AI 对话，不扩大到 restricted、系统管理、
任意模型任务或其他设备。当前 private ceiling 保留，不重新询问整项用途或期限。

源码 `remote/service/mobile-gateway.js` SHA256
`fb69011612e06c86de7e812ca172d0e82b64b18b295b6ae0f9d5a5b9c2f23ad4` 使用
`GRANT_MS = 365 * 24 * 60 * 60_000`，`expiresAt = Date.now() + GRANT_MS`。
每次启动已有受信手机时重新 grant；新配对也建立同样 grant。因此不是首次配对起固定
365 天，而是每次启动重新计算未来 365 天；反复重启可以持续向后延长到期日。
这是当前实现事实，writer 保留这段代码本身不等于续期策略获批。
二维码五分钟有效期是另一层配对引导期限，不是手机 read/edit/capture grant 的期限。

#2021 已披露重启续期；#2027（11:36:20.390Z）紧邻答复，再列“365 天、每次重启续期”
与“从配对起固定期限、重启不延长”。#2028 是这段问题后的真实答复，而非建议回复；
但只明确维持 365 天，没有明确区分上述两个同为 365 天的策略。不能把旧的“尚无期限
答案”继续当现状，也不把上下文推断写成真人明确选了重启续期。

**唯一未明确项：重启是否把到期日重新设为当时起 365 天。** 产品 writer 是原用户
问题的唯一 owner，只闭环此差异并记录答案；C-Staff/controller 不另向真人提问。
不重问已选的 365 天和用途，不默认改成固定到期，也不将现有滚动实现标为已确认。

### 当前会话与后续实施边界

本解释不撤销现有 session，不触发 pair/revoke、重启、回滚、降权或新增部署动作。
尤其 `mobile-gateway.js pair` 会撤销手机信任并重启服务，并非无副作用的状态查询；
不得为了“核实”或重问授权打断用户正在进行的配对。续期解释不取消手机部署目标、
已选的 365 天或 AI 对话用途；仅未确定的重启延长策略保留待决。
已有 grant 变更/到期/撤销检查继续是实现门禁；撤销不能回收已显示数据，也不能回滚
已提交的编辑、已接受的抓取或已经发出的 provider 请求。产品 writer 负责原用户问题与后续代码，C-Staff 只维护
本接口边界。运行中 grant、真实测试与部署结果未在本次独立复验。

## Relay/bridge 条目 AI 对话（2026-11-02 源码合同）

本节对应 `remote/mac/library-router.js` 与 `crates/memex-agent/src/item_chat.rs`，
是现有方法的记录，不新增 daemon/前端写集，也不宣称已完成手机真实模型验收。

### 请求、响应与权限

```json
{"id":"chat-1","method":"memex.library.item.chat","params":{"item_id":"item_fixture","request_id":"mobile_chat_fixture_0001","messages":[{"role":"user","content":"Explain the author's conclusion."}]}}
```

- envelope 仅 `{id,method,params}`；`id` 为 1–128 个 `[A-Za-z0-9_.-]` 字符。
  params 必须恰为 `{item_id,request_id,messages}`，不接受客户端 ceiling、provider、
  system prompt、工具、附件或 `expected_revision_id`。`item_id` 为 1–256 个
  `[A-Za-z0-9_-]` 字符；`request_id` 为 16–128 个可见 ASCII 字符（0x21–0x7e）。
- `messages` 为 1–40 个恰含 `{role,content}` 的对象，role 仅 `user|assistant`，
  最后一条必须为 user；每条 UTF-8 content 最多 16 KiB、trim 后非空且不含 NUL，
  总 content 最多 64 KiB。还须满足序列化帧上限：router 请求 96 KiB，当前 UDS
  command 帧 64 KiB（含 JSON 开销），故总 content 的理论上限不保证整帧可发送。
- 当前实现没有单独 `chat` scope：有效 grant 的 `read` 即可进入此方法，
  不要求 `edit`。这意味着当前 `read` grant **不能再描述成纯只读、无模型外传**。
  此轮 owner 手机的 AI 对话用途已覆盖；不得据此给其他只读用途静默继承模型调用权。
- router 注入 grant 的 `max_sensitivity`，转换为 daemon
  `item_chat {item_id,max_sensitivity,request_id,messages}`；daemon 按条目与当前 revision
  的有效敏感级别读取。隐藏与不存在均返回 `library_resource_not_found`。
  返回前再次检查 grant 当前性/到期；该检查不能取消已发出的 provider 请求。

成功 envelope 为 `{id,result}`，result 恰含下列字段；`model` 是实际 route 的标签：

```json
{"id":"chat-1","result":{"request_id":"mobile_chat_fixture_0001","reply":"Synthetic answer.","model":"configured-chat-model","finish":"stop"}}
```

`finish` 为 `stop|length`；length 表示达到生成长度限制，不冒充完整回答。失败 envelope
为 `{id,error:{code,message}}`，message 等于脱敏 code。相关 code 包括
`invalid_request|invalid_chat_request|library_resource_not_found|no_chat_provider|provider_failure|provider_outcome_unknown`，
以及 session/权限、daemon 不可用、内部错误与响应过大错误。

### 数据流、重发与已知限制

- 显式发送一次请求调用保存配置的 chat purpose route，经既有 Keychain/CLIProxyAPI
  provider 路径；没有 Ollama、后台聊天或从手机传凭据的分支。会向该 provider 发送
  messages、所选当前 revision 的标题/来源 URL/有限元数据、已有成功的 AI minutes，
  以及正文上下文。标题加 Markdown 不超过 160 KiB 时取全文，否则取标明省略中间的
  首尾节选；不是保证全文推理。元数据块最多 4 KiB。
- 这是非流式单轮请求，daemon 另线程执行；router 等待 UDS 最多 130 秒。
  当前响应不含 revision_id，也不接受 expected_revision_id，每轮重新读当前 revision，
  因而不能承诺跨轮锁定同一版本或将回答当作 revision-bound 引用证据。
- daemon 不持久化对话/回复，不提供聊天历史、取消或结果查询；`request_id` 仅校验并
  回传，**不是幂等键**。同 ID 再发仍可能产生新调用/费用；禁止自动重发或以重新配对
  恢复未知结果。`provider_outcome_unknown` 不自动重试；超时、断链、到期/撤销后丢弃
  回复，也不能证明 provider 未执行。当前 router 对 chat 的传输错误没有统一映射为
  unknown；`daemon_unavailable` 等错误不得被客户端当成安全重试证明。
- `memex.session.info` 当前返回 `chat:"available"`、`generation_ready:false`、
  `translation:"unavailable"`。chat available 只表示路由存在，不证明配置、凭据、
  provider 可用或真实生成成功；不可绕过 `no_chat_provider` 等实际结果。

## Human routes

```text
GET|HEAD /library
GET|HEAD /memex
GET|HEAD /capture
GET|HEAD /library/items/<item_id>/read?theme=<light|dark>&font=<sans|serif>&language=<zh|en|zh-en|en-zh>
GET|HEAD /library/items/<item_id>/attachments/sha256-<digest>.<gif|jpg|png|webp>
POST /memex/capture
GET /memex/capture/status/<job_id>
```

`/library` 接受唯一、非空、UTF-8 query fields：`limit`、`cursor`、`tag`、`detail`、`theme`、
`font`。`limit` 是 canonical decimal `1..=100`，默认 50；cursor 必须是当前过滤结果中已存在
的 Item id；tag 必须是 startup snapshot 中可见的 exact manual tag。未知、重复、空或非法字段
返回 generic `404`。

`/library` 的 source renderer 另包含显式版本
`mobile_library_homepage_capture_v1` 的“抓取一篇文章”card。它只用无 `action`、无 `name`、
`autocomplete=off` 的 textarea/form 和 external `mobile-library-v0.js` enhancement；CSP 保持
`form-action 'none'`。JavaScript 把 `{schema_version:1,command:"capture_public_url",text}` 作为
same-origin JSON body POST 到 `/memex/capture`，并使用不从正文派生的 opaque
`Idempotency-Key`。正文不进入 query、history、storage、console、analytics 或 response；成功后
立即清空 textarea，只显示 opaque job id/state 与 host/path-class/query-present redaction。

Client 的 16 KiB/exact-one-scheme 检查只是 bounded UX；server 必须按
`interfaces/memex.local_capture_agent.md` 重新执行完整 exact-one-URL validation 和 idempotency。
Loopback/orb read mode不实现POST；private-LAN direct mode把canonical URL与opaque key通过owner-only
UDS `Command::Enqueue`写入`local_surface` producer namespace，绝不直接打开queue SQLite。Exact replay
返回同一opaque job，identity conflict返回409，daemon不可用返回503。Browser随后读取同源status，
只显示真实`queued|fetching|processing|needs_user|failed|stored`；stored后进入live `/memex`。
导出文件名仍为
`pages/index.html`、`assets/mobile-library-v0.css`、`assets/mobile-library-v0.js`，但 exporter 必须
按变更后的 bytes 重算既有 `files[].bytes` / `files[].sha256` 与 bundle digest，不能复用旧 asset
digest。Homepage version 只由 `pages/index.html` 内的 `data-homepage-version` 承载并受该文件 digest
保护；本变更不增加或修改 export manifest field/schema。

点击 row 打开 server-rendered metadata sheet；fine pointer 双击和明确的“阅读全文”链接进入
reader。移动端不依赖双击。Reader 丢弃 raw HTML 与外部/非法 image destination，仅允许 fragment、
root-relative non-network-path 以及 valid `http`、`https`、`mailto` links。只有 current revision
实际引用、Evidence-backed、size/media/signature/SHA-256复核通过的 captured image attachment可由
same-origin route读取；missing/corrupt/hidden attachment fail closed。

省略 `language` 是有效的 canonical source-authority reader mode，不是 malformed request；`theme`与
`font`仍须保留。预渲染 adapter 必须为该省略形式保留 source projection，或在导出时从同一次
verified ItemRevision / AnalysisRecord readback把每个 emitted reader link解析成对应的显式语言
projection；不得一边保留无 `language` 的 list/detail/double-click link，一边把 downstream route
收窄为 required `language`。Source HTML使用host-agnostic root-relative `/library`、`/assets`路径；
deployment adapter可将它们统一映射到一个外部配置的canonical mount，但必须同时覆盖list/detail、
reader、CSS/JS与attachment，不能硬编码旧站host或只改其中一类路径。

如果当前 exact ItemRevision 具有唯一、同 source language、ceiling-visible 的标题与全文译文
Analysis，reader提供中文、English、中/英逐段、英/中逐段四种普通 GET 模式；摘要是可选衍生，
不阻塞完整标题/全文 translation pair。metadata sheet显示可追溯AI标题/摘要。任一
purpose/language存在多条候选时不静默选择，双语模式fail closed，原文仍可读。原文
ItemRevision始终是authority；AI文本只从带model/model digest/prompt/revision provenance 的
immutable AnalysisRecord读取。

若 exact revision 具有 async translation job，metadata sheet 与 reader 以
`data-translation-state`显示真实 `queued|running|failed|succeeded`；queued/running文案为“正在翻译”，
failed保留原文并仅在job可重试时说明可安全重试，不显示假百分比。只有job succeeded且上述唯一
title/document pair重新校验通过才显示“翻译完成”并开放中文模式；partial frozen/output/record均不
展示。对job-owned结果，重新校验还必须证明surface所选title/document Analysis ID与该succeeded
job提交的两个ID完全一致。无queue的既有合法Analysis pair保持兼容。

若exact revision具有async summary job，metadata sheet与reader独立显示
`data-summary-state=queued|running|failed|succeeded`。只有job `succeeded`且唯一Summary
Analysis的revision、job ID、model digest与prompt provenance全部重新校验通过，摘要正文才显示；
queued/running/failed/unavailable从不暴露冻结输出，也不改变原文或translation可读性。状态API为
`GET|HEAD /api/v2/items/<item_id>/summary`，仅返回body-free job view。

Library 与 reader 的颜色 token 仅使用无彩色灰度。`theme` 只接受 `light` 与 `dark`，分别对应
日间与夜间；`sepia` 和其他值 fail closed 为 generic `404`。日夜切换使用普通 GET links，JavaScript
只负责保存 local preference 和同步 active state，无脚本时仍可切换。

## Local JSON routes

```text
GET|HEAD /api/v2/items?limit=<1..100>&cursor=<item_id>&tag=<manual_tag>
GET|HEAD /api/v2/tags?limit=<1..100>&cursor=<tag>
GET|HEAD /api/v2/items/<item_id>/metadata
GET|HEAD /api/v2/items/<item_id>/content
GET|HEAD /api/v2/items/<item_id>/translation
```

- item list 是 body-free compact summary：identity、type、title、creators、published/captured time、
  site、manual tags 与 effective sensitivity；
- metadata 返回 Store-owned integrity-checked `ItemReadMetadata` 与 canonical sourced typed
  metadata，不返回 Markdown、Evidence ids、provenance activities、attachments 或 backend refs；
- content 返回 identity、title 与 Markdown，不复制 metadata；
- translation 返回 exact current item/revision 与可空的body-free job view；job view只含identity、
  source/target language、state/reason、attempt/retryable与timestamps；
- hidden 与 missing detail 都返回相同 generic `404`；corrupt visible record 返回 generic `500`；
- `/api/v1` schema、routes 与 `surface_version=local_v0` 保持不变。

## Snapshot and memory

每次library/items/tags list read逐条调用 Store owner read seam：先验证 Item header + integrity manifest，再执行
sensitivity ceiling；只有可见 Item 才打开、hash 验证 revision。每条完整 revision 验证后立即丢弃
正文、Evidence、provenance 与 attachments，只保留 compact summary。

Snapshot 按 capture time descending、Item id descending tie-break 排序；tags 只来自 canonical
manual tags。Retained row/tag Vec capacity、nested Vec slots 与 String capacity 的计算上限为 64 MiB；
这不是 process RSS、filesystem page cache 或 startup transient 的上限。

List snapshot在每次request重新构建，因此daemon Store成功后无需重启或static export即可在
`/memex`出现。单次request使用一个完整snapshot；失效/unsafe Store不伪装为空资料库。

## Security boundary

- 默认bind仅`127.0.0.1`；private-LAN mode只接受RFC1918 non-loopback bind、stable high port与exact
  `<bonjour.local>:<port>` Host/Origin，拒绝wildcard/public bind；
- `--orb-portal` 是仅限 Amp orb 的显式 demo opt-in：要求 `AMP_ORB=1`，并只从 Amp 注入的
  `PUBLIC_URL` 接受 exact HTTPS `*.onamp.dev` authority/origin；Portal 鉴权由 Amp 外层提供，
  MemeX 本身不因此获得 production auth/relay 能力；
- optional Origin 必须与当前 accepted Host 的 origin exact一致；无 CORS；除direct mode exact capture
  POST外均为GET/HEAD only；
- mobile HTML 使用 self-only CSS/JS/connect CSP 与 `form-action 'none'`；response kind 由 route
  指定，不能由正文改变；
- 唯一 attachment bytes例外是 digest-addressed、current-revision、Evidence-backed且重新验证
  size/media/signature/SHA-256的mobile captured-image route；无 generic raw/path route；
- 除direct UDS enqueue外无write；无raw HTML、外部图片、filesystem path、public bind、production relay、
  WebDAV、OCR、deployment 或 production domain operation。
