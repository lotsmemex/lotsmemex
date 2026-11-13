# MemeX capture mailbox v1 dormant primitives

This document records the dormant, transport-free wire boundary extracted from
historical commits `6660591db704db80ff1c76e61d71c63472a05fda` and
`96b926b7583d81bd180e2a829cea22125ffde01c`. The current Dashboard wire
authority is the shipped `f1eba4c8eef0244a3e0d993649922bafac30d8fa` main tree.
The historical relay runtime is not imported by this change.

## Scope

`memex-agent::mailbox` contains only:

* bounded canonical JSON types for command, lease, heartbeat, progress,
  acknowledgement, negative acknowledgement, status, pairing, and fence values;
* strict unpadded base64url, identifier, nonce, lease, cursor, fence, state, and
  body-size validation;
* the frozen HPKE `DHKEM_X25519_HKDF_SHA256/HKDF_SHA256/AES_256_GCM` command
  opener and `memex-command-commitment-v1` HMAC;
* the exact `memex-mailbox-hmac-v1` canonical authentication input, body digest,
  and signature header builder.

Keys, timestamps, and nonces are caller-provided values. Tests use only the
synthetic values in the fixture. No clock, entropy source, Keychain, HTTP
transport, queue mutation, polling loop, daemon/IPC caller, publication
execution, or Dashboard origin is present here. The module is public for a
future owner-controlled adapter, but the current production path has zero
mailbox callers and current Agent configuration is unchanged.

## Dormancy and existing authority

These primitives do not enqueue, poll, heartbeat, progress, ACK, NACK, or
activate a cloud relay. They do not authorize cloud activation. Current
credentialless authority, direct intake, and local pairing behavior remain
unchanged. A future relay must be separately reviewed for transport, secret
ownership, lease scheduling, durable state, and publication/readback gates.
There is no Agent → Surface dependency.

The source history's runtime types are intentionally not carried over: the
historical `RelayClient`, queue/runtime state, transport, Keychain resolver,
clock polling, and publication code remain outside this dormant boundary.

## Synthetic conformance fixture

`interfaces/fixtures/memex-capture-mailbox-v1-vectors.json` is copied byte for
byte from the source-first wire authority in historical commit
`96b926b7583d81bd180e2a829cea22125ffde01c`, which is byte-identical to the
Dashboard `f1eba4c8eef0244a3e0d993649922bafac30d8fa` fixture.

* bytes: `14007`
* SHA-256: `fa28def681f6db56af0b3cfccc210561bf2339e75e26c50681ea27d5e16026f7`
* provenance: historical `96b926b` / Dashboard `f1eba4c` fixture; synthetic
  warning is part of the fixture and its keys/nonces must never be used at
  runtime.

The tests require exact canonical JSON bytes, HPKE plaintext opening and
commitment, body digest and signature bytes. They also reject unknown fields,
non-canonical JSON, padded base64url, malformed URLs, oversize bodies, mutated
command/producer/key/AAD/ciphertext/tag/commitment values, invalid lease/fence
values, and errors that disclose URL, plaintext, key, or ciphertext material.
