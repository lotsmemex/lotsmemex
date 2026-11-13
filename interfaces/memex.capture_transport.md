# memex.capture_transport

## Summary

`memex intake serve` 的 authenticated IPv4-loopback capture endpoint 同时接受：

```text
application/json
application/vnd.memex.capture-v1
```

前者保留 One-click Capture V1 的 inline HTML/base64 image compatibility；后者是 connector-ready
length-prefixed binary envelope，直接传 HTML 与 image raw bytes。两种 transport 必须归一化为相同
`VerifiedCapture`，transport identity 不进入 canonical Store graph 或 replay fingerprint。

## Binary envelope V1

```text
offset        bytes                   value
0             8                       4c 4f 4c 49 54 41 00 01
8             4                       manifest length, unsigned big-endian u32
12            manifest length         UTF-8 manifest JSON
12 + M        html.byte_size          raw HTML bytes
...           images[0].byte_size     raw image 0 bytes
...           images[N].byte_size     raw image N bytes
```

没有 delimiter、part header、filename、padding、footer 或 epilogue。Raw body 顺序固定为 HTML，
然后按 manifest `images` 顺序排列。Manifest 使用 JSON transport 的同一 capture/metadata/remote
attachment fields，但：

- `html` 只含 `media_type`、`byte_size`、`sha256`，不含 `body`；
- 每个 image 只含 `source_url`、`media_type`、`byte_size`、`sha256`，不含 `body_base64`。

Outer `Content-Length` 必须严格等于：

```text
12 + manifest_bytes + html.byte_size + sum(images[*].byte_size)
```

## Browser connector construction

Privileged extension background/service worker 可用 finite `Blob` 构建 request；不能使用会产生
chunked transfer 的 `ReadableStream`，也不能由 JavaScript 设置 forbidden `Content-Length` header：

```js
const manifestBytes = new TextEncoder().encode(JSON.stringify(manifest));
const prefix = new Uint8Array(12);
prefix.set([0x4d, 0x45, 0x4d, 0x45, 0x58, 0x00, 0x00, 0x01]);
new DataView(prefix.buffer).setUint32(8, manifestBytes.byteLength, false);

const body = new Blob(
  [prefix, manifestBytes, htmlBytes, ...imageBytes],
  { type: "application/vnd.memex.capture-v1" },
);

await fetch(intakeUrl, {
  method: "POST",
  headers: {
    "Content-Type": "application/vnd.memex.capture-v1",
    "X-MemeX-Intake-Secret": pairingSecret,
    "X-MemeX-Intake-Nonce": nonce,
  },
  body,
  credentials: "omit",
  cache: "no-store",
  redirect: "error",
  referrerPolicy: "no-referrer",
});
```

Pairing secret 只能留在 privileged extension storage/background context；不得进入 page DOM、content
script payload、Cookie、Authorization、query、manifest、log 或 receipt。Browser 实际生成的
Content-Length、Origin、PNA/CORS 与 service-worker behavior 仍需独立 real-browser proof。

## Bounds and rejection

- outer request：36 MiB；manifest：4 MiB；HTML：8 MiB；
- images：最多 32；单张 2 MiB；aggregate 20 MiB；
- creators 64、identifiers 64、manual tags 256、remote attachments 128，在 deserialization 时停止；
- exact content type，不接受参数、case variant 或近似 subtype；
- exact canonical decimal Content-Length；拒绝 TE、Upgrade、Expect、Content-Encoding、Trailer、
  Cookie 和 Authorization；
- magic/version、manifest length、UTF-8/JSON/schema、unknown/duplicate fields、computed total、
  body size/hash/media/magic/source URL 与 duplicate image identity 任一不符即 `400`；
- 不接受 trailing byte；read path 从 outer Content-Length bounded reader 读取 exact component length。

完整 body 验证成功后才 reserve nonce。验证失败不消耗 nonce；验证完成后 translator/CAS/Store
失败仍消耗 nonce。Nonce set 由 JSON/binary transport 共享。

## Authority and privacy

Binary envelope 是 transport，不是 Evidence、canonical manifest、portable item package 或 public
export。Translator 仍无网络/文件权限；Intake 只构造 verified runtime bytes；Store 仍是
Item/Revision/Evidence/receipt 的唯一 writer。

Endpoint 继续只 bind `127.0.0.1`，要求 exact configured Origin 和 pairing secret。本合同不授权
读取 Cookie、Authorization、browser profile、private page、登录态或 arbitrary local file，也不
授权公网 bind、部署、WebDAV、OCR/PDF provider 或 third-party sync。
