# Explicit public-article prompt trial

This is an operator-only backend experiment, not a desktop/UDS command, a
readiness flag, a queued translation, or a Conversation implementation. Its
translation-baseline-v1 uses the provider-neutral faithful translation prompt;
it does not depend on an Ollama runtime, worker or config. Production local-model
execution has been removed. Existing generated records/provenance are retained;
this trial neither migrates them nor changes queue retry rules. No model-settings
save implicitly enables this path.

`memex-analysis` owns two additional opt-in prompt profiles:

- `classical-translation-v1`: faithful modern Simplified Chinese. Classical style
  means clear shared attention, not archaic prose. Preserve first-person voice,
  paragraph order, argument, uncertainty, technical relationships, irony and
  existing metaphors. Do not insert the editor's explanations or sensory details.
- `grounded-answer-v1`: concise answer first, evidence identifiers next to claims,
  exact original quotations distinguished from translated quotations, explicit
  missing evidence and separation of allegations, inference and suggestions.
  Input-local `[P…]` labels are not canonical revision/byte citations.

These are new versioned profiles, not a migration of previously generated text.
Summary and baseline faithful translation prompts remain distinct from these
opt-in profiles; retiring a model transport does not select a new prompt. There was no
previous product Q&A prompt: `answer-control-v1` is a minimal experimental control,
not a validated historical baseline. A trial receipt records the exact prompt hash.

## Controlled host

Build `cargo build -p memex-agent --example prompt_trial`. The example takes
seven explicit positional arguments:

```text
prompt_trial ABS_DB REVISION GENERATION PROFILE ABS_APPROVED_INPUT SHA256 ABS_NEW_RECEIPT
```

Profiles: `translation-baseline-v1`, `translation-classical-v1`,
`answer-control-v1`, `answer-grounded-v1`. The caller must have explicit permission
for the input's disclosure and the live call. An input hash verifies identity,
not authorization. Use an owner-private absolute input file and receipt directory.
No path under a particular user's home is a product default.

The example reads only the namespaced model settings table with SQLite read-only
and query-only flags, requires the specified revision and generation, and accepts
only the saved Translation route `local-cliproxy`, literal
`http://127.0.0.1:8317/v1/`, `gpt-6-astra`, explicit `low`. The Q&A experiment uses
this same explicitly authorized target without changing the saved Chat route.
It never opens Queue/Store, touches library text, ingests, or runs workers.

The existing scoped Keychain resolver supplies the secret directly to the backend
transport. No raw key appears in arguments, files, stdout, renderer or receipts.
Inaccessible Keychain stops before HTTP; no UI, alternate key, credentialless
fallback, reprovisioning, ACL change or re-signing is attempted.

Each invocation creates one new private JSONL receipt before resolving credentials.
It fsyncs `dispatching_outcome_unknown` before calling the provider. Existing
receipt paths are refused. On an interrupted call, retain the receipt and reconcile;
choosing a new filename is not permission to replay an unknown request. Preflight
`not_dispatched` is distinct from any post-dispatch error, which may have consumed
tokens even if there is no text or usage to report. Failures never auto-retry.

## Transport and acceptance boundary

`memex-provider::generation::complete` issues one non-streaming
`POST chat/completions`, using the exact captured model and effort. It disables
proxies, redirects and retries, sends no tools, caps source at 64 KiB, system prompt
at 16 KiB, encoded request at 128 KiB, response at 1 MiB, and total time at 120 s.
This independent generation deadline does not change the five-second UDS timeout.
The trial caps completion tokens at 10,000 for translation and 2,500 for answers;
these are requested token bounds, not a currency ceiling or proof of billing.

Acceptance requires exactly one assistant choice, matching reported model,
`finish_reason=stop`, nonempty text, no refusal/tool call, followed by unchanged
settings/credential binding in the host. Model equality is only the provider's
reported identity, not immutable-weight attestation; effort is the sent preference,
not independently verified internal execution. Missing usage remains unknown;
pricing is not inferred. Raw provider errors/headers are never exposed.

Transport acceptance is not semantic acceptance. Inspect the actual output against
the fixed source: paragraph/URL/code preservation, numeric subjects and units,
conditional claims, authorship/attitude, missing facts and citation validity.
One article and one sample per profile cannot establish general superiority or
prompt-injection resistance. Do not put trial output into canonical records,
enable GUI Send, or activate whole-library inference based on these receipts.

Rollback: stop using the opt-in example/profiles. No settings, canonical data,
existing service, installed binary, credentials or schema require rollback.
