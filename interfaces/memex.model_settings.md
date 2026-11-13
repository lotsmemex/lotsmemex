# memex.model_settings — local configuration slice

Owner: backend C-Staff. `memex-models` owns provider/model configuration and
purpose selection; the agent hosts its namespaced table in its existing secured
database. The desktop owns only presentation. No dependency on UI, canonical
library, model transport, remote gateway or platform account.

## Owner-only UDS

```json
{"command":"model_settings_read"}
```

On success the existing envelope adds `model_settings`:

```json
{
  "schema_version": 1,
  "revision": 0,
  "configuration": {"providers": [], "models": [], "selections": []}
}
```

Replacement is one atomic compare-and-swap, not three independently saved lists:

```json
{
  "command": "model_settings_replace",
  "expected_revision": 0,
  "configuration": {
    "providers": [{"id":"local-proxy","label":"Local proxy","api_base":"http://127.0.0.1:8317/v1/"}],
    "models": [{"id":"astra","label":"Astra","provider_id":"local-proxy","model_id":"gpt-6-astra"}],
    "selections": [{"purpose":"chat","model":"astra"}]
  }
}
```

This example is not preinstalled or claimed reachable. A successful replacement
returns the new snapshot and increments revision. `stale_model_settings` leaves
the existing configuration unchanged; reload and reconcile instead of overwriting.
An unknown transport outcome must be read back, never retried with an invented
revision. Old commands and errors omit `model_settings`, preserving old decoders.
An old daemon may return `internal_error` for this unknown command; treat it as
unavailable, not permission to write a separate UI-side model database.

- Purposes: `chat`, `suggestions`, `translation`, `summary`, `classification`. Each has zero or one
  selected model. No implicit fallback. No selection means unconfigured.
  `summary` serves minutes and every AI insight kind (highlights, quotes and
  fact_check, including their availability flags); `chat` serves item chat only.
  Fact check no longer borrows the chat route.
- Each selection may carry `reasoning_effort`: `none`, `minimal`, `low`, `medium`,
  `high` or `xhigh`. Absent/null means unspecified and is omitted on serialization;
  explicit `none` is distinct. This is a per-purpose request preference, not a
  provider capability claim. Unknown values reject replacement, never coerce.
  The personal translation preference is Astra with `low` until comparative
  retesting; users may select BYOK. Saving it does not authorize unrequested jobs
  or grant permission to send private content. Classification saves a future
  fast-decision route; it does not execute bulk classification.
- Provider ID/model-card ID: unique within their list, 1–80 ASCII letters, digits,
  dot, underscore or hyphen. Upstream `model_id` is a separate exact string,
  1–256 bytes without whitespace/control characters. Labels are 1–256 UTF-8 bytes,
  nonblank and without control characters.
- Base URL: canonical spelling with a trailing slash; HTTPS or HTTP at literal
  `127.0.0.1`/`[::1]`. No userinfo, query, fragment or zero port. `localhost` HTTP
  is not normalized implicitly. A local proxy may forward to a remote provider.
- Bounds: 32 providers, 128 model cards, five purpose routes, serialized
  configuration at most 48 KiB. Existing UDS frame bounds remain unchanged.
- Deletion is removal in the replacement snapshot. A selected model cannot be
  removed unless its routes are cleared/replaced in the same transaction. A
  provider with zero models may remain. Duplicate IDs/routes and dangling links
  reject the whole change with `invalid_model_settings`.

## Configuration is not execution availability

This slice saves **non-secret configuration only**. There is no API-key/ref field,
credential import, inference test, chat generation or billing in settings save.
Unknown fields (including `api_key`) are rejected. Never put secrets in labels,
IDs or URLs. Settings are local configuration, not a portable library export.

Do not enable Send/suggestions solely because a model is selected. A manually
entered ID is unverified; `/models` discovery would establish only that an
endpoint advertises an ID, not inference capability or authorization to transmit
library text. Discovery evidence must bind endpoint and credential generation,
and future execution must bind the exact settings revision/target at dispatch.
The saved-provider translation worker resolves settings at dispatch, then freezes
the adapter descriptor for that attempt. Queue admission alone does not pin the
model; changing a selection can affect jobs not yet dispatched. Do not present
queue-time model selection as an already frozen execution receipt.

Credential storage/rotation and explicit discovery are separate commands below.
Saved-provider translation/summary execution and `translation_provider_v1` are
documented in `memex.async_translation.md`, including explicit `translation_request`,
per-job provider_authorized gating and bounded retries. Capability availability is
not inference success or disclosure authorization. Conversation persistence and
other execution features are not implied by that translation contract.
Model settings plus the scoped Keychain credential are now the sole generation
configuration, using the configured CLIProxyAPI provider route. Ollama execution
has been removed at the user's request; no alternate local-model worker or fallback
remains. The provider digest identifies routing configuration, not immutable model
weights. No automatic transfer of CLIProxyAPI keys occurs.

The retired top-level daemon-config `translation` block is accepted but ignored.
A non-null block causes one fixed startup notice, never a dump of its contents;
config serialization omits it. Reading an old config does not rewrite it, migrate
its values into model settings, start a local model or supply a credential.
There is no background_generation switch. Unauthorized reconciled translation/
summary jobs display not_requested; saving settings cannot authorize a library sweep.

## Local credentials, separate from model settings

These commands require the same owner-only UDS and an existing configured provider.
All carry `expected_revision` of model settings, not a client-supplied endpoint.

```json
{"command":"provider_credential_read","expected_revision":1,"provider_id":"local-proxy"}
{"command":"provider_credential_replace","expected_revision":1,"provider_id":"local-proxy","expected_generation":0,"api_key":"<user-entered key>"}
{"command":"provider_credential_clear","expected_revision":1,"provider_id":"local-proxy","expected_generation":1}
```

Only replacement accepts a secret. The native desktop may hold the entered value
transiently to send this owner-local command; clear the input after submission or
scope change, never persist/log it, and never send it through a mobile gateway.
There is no raw-key read command. Responses add only `provider_credential`:

```json
{"settings_revision":1,"provider_id":"local-proxy","api_base":"http://127.0.0.1:8317/v1/","generation":1,"state":"saved","masked_key":"***abc"}
```

`state` is `missing`, `saved` or `scope_changed`, never a connection test.
Only the last three ASCII characters may be exposed; keys of three or fewer
characters are fully masked. Missing/scope-changed values have `masked_key:null`.
Keys are 1–4096 visible ASCII bytes, with no whitespace/control characters.
Changing a provider's base URL invalidates use of its stored key; it never sends
the old key to the new endpoint. Another provider at the same URL has a separate
key. A clear removes the secret and keeps a generation tombstone, preventing an
old save/clear from replaying against a reset generation. Settings selection and
credential generation are independent; saving a key does not choose a model.

Storage is the non-iCloud-synchronized macOS Keychain service
`org.memex.provider-credentials.v1`, scoped by the trusted canonical agent DB
path and provider ID, not a renderer-supplied account or arbitrary Keychain ref.
The secret does not enter SQLite, canonical library, logs or MemeX portable
exports. Moving the agent DB does not migrate credentials; the old Keychain entry
is retained. Removing provider configuration does not delete its Keychain entry;
clear the credential before removing the provider if deletion is intended.
Recreating the same provider ID at the same DB path and exact URL can reuse it.
Rollback leaves Keychain entries intact; older binaries do not access them.

SQLite's immediate transaction serializes credential CAS and settings replacement
across hosts using this DB; the Keychain value is a single atomic replacement,
not a distributed SQLite/Keychain transaction. A lost response is unknown: read
status before any repeat. `stale_model_settings` or `stale_provider_credential`
does not change the key. Locked/inaccessible Keychain, unsupported OS or native
errors return `credential_store_unavailable`; corrupt entries fail closed. The
daemon suppresses automatic Keychain UI for each native operation, restoring the
prior process-local flag afterwards. It never creates a plaintext fallback or
prompts from a background request. Signing/ACL behavior after packaging still
needs native release validation; local tests do not prove all deployment contexts.

## Explicit model discovery, not inference

```json
{"command":"provider_models_discover","expected_revision":1,"provider_id":"local-proxy","authorization":{"mode":"saved_key","expected_generation":1}}
```

For an intentionally unauthenticated endpoint, use
`"authorization":{"mode":"credentialless"}` explicitly. Missing/inaccessible
saved keys never fall back to credentialless or another provider. Discovery is
an explicit user action, not a side effect of opening settings, saving or switching.
It issues only `GET <api_base>models`, with no prompt, library text, inference or
retry. No environment proxies, redirects, cookies or automatic decompression;
hostname-verified TLS for HTTPS, or the already validated literal loopback HTTP.

The owner's explicit request to auto-configure the local CLIProxyAPI authorizes
this local provisioning/discovery step. It does not authorize automatic inference
over the imported library. The proxy owner supplies a MemeX-only credential;
other applications' keys and Amp account settings are not copied or changed.

Success adds `provider_models` with `schema_version:1`, `settings_revision`,
`provider_id`, `api_base`, `credential_generation` (null for explicit
credentialless) and sorted/deduplicated `model_ids`. Treat them only as advertised
IDs; do not infer pricing, tools, context limits, account entitlement or working
generation. Display names remain separate from exact upstream IDs.

The network operation has a three-second total deadline, 1 MiB response bound,
at most 1024 entries, and the same ID validation as settings. Global UDS response
limits still apply (`result_too_large` rather than a truncated result). Error
codes are `provider_unauthorized`, `provider_redirect_refused`,
`provider_unavailable`, `provider_timeout`, `provider_response_too_large`,
`invalid_provider_response`, plus credential/settings errors. Raw provider bodies
and transport diagnostics are never returned. The DB lock is released during
network I/O; captured settings and credential generation are rechecked before
accepting success. The desktop must also discard late results after its current
scope/selection changes. The daemon's general serialized-request limitation is
not fixed by this slice; no new inference or Send availability is implied.

Old commands and errors omit both new fields. The fixed P2b GPUI supply binary
predates these credential/discovery commands; it has not been silently replaced.

## Persistence and rollback

The first save creates `memex_model_settings_v1` in the agent database, outside
the canonical library. Reads of an unconfigured database return revision zero
without creating this table. Writes use an immediate SQLite transaction. Existing
database path/owner safeguards and owner-only UDS authentication still apply.
Unknown schema versions fail closed. Rollback to the older daemon leaves the
additional table intact and unused; no library migration or secret deletion.

This is not a remote management API. Do not forward these owner-only commands
through the future mobile gateway without its explicit permission contract.
