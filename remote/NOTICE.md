# Third-party notice

The focused transport implementation in this directory adapts patterns from:

- **Remodex**, <https://github.com/Emanuele-web04/remodex>
- Reference commit: `8105e25038ad07443da01cda6c21e5f77ec61cd5`
- Copyright 2025–2026 Emanuele Di Pietro
- License: Apache License 2.0
- Exact license copy from that pinned commit: [`LICENSE-remodex-apache-2.0.txt`](LICENSE-remodex-apache-2.0.txt)

Adapted areas are identified in source comments. The implementation was substantially modified for MemeX: the relay is reduced to content-blind one-Mac/one-phone WebSocket routing; bootstrap authentication and relay admission are separate; metadata is authenticated as AES-GCM AAD; schemas, sizes, and counters are strict and bounded; the install-free diagnostic uses native WebCrypto and non-extractable IndexedDB keys while explicitly trusting HTTPS/static-code delivery; Codex dispatch and product features were replaced by three authorized MemeX owner-UDS RPCs; and account, subscription, push, provider, and deployment systems were omitted.

The authenticated in-page QR scanner uses **jsQR 1.4.0**, <https://github.com/cozmo/jsQR>,
licensed under Apache License 2.0. The exact dependency is pinned in `package-lock.json`; the Apache 2.0
license text is the same standard text preserved in [`LICENSE-remodex-apache-2.0.txt`](LICENSE-remodex-apache-2.0.txt).
