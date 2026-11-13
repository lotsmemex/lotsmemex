'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { randomBytes, createHash } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { startChromeFixture } = require('./chrome-fixture');

test('real Chrome intake preserves capture identity, independent comments and checked provenance', async (t) => {
  const fixture = await startChromeFixture();
  t.after(() => fixture.close());
  async function post(route, payload, extra = {}) {
    const response = await fetch(new URL(`api/v1/${route}`, fixture.baseUrl), {
      method: 'POST', headers: { Origin: fixture.origin, 'Content-Type': 'application/json',
        'X-MemeX-Intake-Secret': fixture.secret, 'X-MemeX-Intake-Nonce': randomBytes(32).toString('hex'), ...extra },
      body: JSON.stringify({ schema_version: 1, ...payload }),
    });
    return { status: response.status, body: await response.json() };
  }
  const capabilities = await post('capabilities', {});
  assert.deepEqual(capabilities.body.features.slice().sort(), ['comments-v1', 'quality-check-v1',
    'url-jobs-enrich-v1', 'url-jobs-no-generation-v1', 'url-jobs-v1']);
  assert.equal((await post('capabilities', {}, { Origin: 'https://foreign.example.invalid' })).status, 403);
  assert.equal((await post('url-jobs/status', { job_id: 'job_missing' })).body.error, 'resource_not_found');
  await fixture.pause();
  const queued = await post('url-jobs/enqueue', { request_key: `chrome_capture_${randomBytes(24).toString('base64url')}`,
    url: 'https://queued.example.invalid/article', include_comments: false, quality_check: 'configured' });
  assert.equal(queued.body.quality_check.state, 'pending');
  assert.equal(queued.body.quality_check.reason, 'awaiting_capture');
  const cancelled = await post('url-jobs/cancel', { job_id: queued.body.job_id });
  assert.equal(cancelled.body.quality_check.state, 'unavailable');
  assert.equal(cancelled.body.quality_check.reason, 'capture_not_committed');
  await fixture.resume();
  let sawRunning = false;
  async function settle(job) {
    for (let n = 0; n < 160; n++) {
      const { body } = await post('url-jobs/status', { job_id: job.job_id });
      sawRunning ||= body.quality_check?.state === 'running';
      if (!['queued', 'leased', 'artifact_ready'].includes(body.state)) return body;
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    assert.fail('synthetic job did not settle');
  }
  for (const [host, quality] of [['capture', 'passed'], ['quality-correct', 'corrected'],
    ['quality-review', 'needs_review'], ['quality-fail', 'failed'], ['quality-unavailable', 'unavailable']]) {
    const payload = { request_key: `chrome_capture_${randomBytes(24).toString('base64url')}`,
      url: `https://${host}.example.invalid/article`, include_comments: true, quality_check: 'configured' };
    const first = await post('url-jobs/enqueue', payload);
    assert.equal(first.status, 200);
    const settled = await settle(first.body);
    assert.equal(settled.state, 'succeeded');
    assert.equal(settled.comments.state, 'captured');
    assert.equal(settled.quality_check.state, quality);
    assert.equal((await post('url-jobs/enqueue', payload)).body.job_id, settled.job_id);
    assert.equal((await post('url-jobs/enqueue', { ...payload, include_comments: false })).body.error, 'request_identity_conflict');
    const revision = JSON.parse(fs.readFileSync(path.join(fixture.root, 'Library/items', settled.item_id, 'revisions', `${settled.revision_id}.json`)));
    assert.equal(revision.sensitivity, 'internal');
    assert(!revision.content_markdown.includes('COMMENT-SYNTHETIC'));
    assert(!revision.content_markdown.includes('Invented article facts'));
    assert(revision.content_markdown.includes('careful reading'));
    const record = JSON.parse(fs.readFileSync(path.join(fixture.root, 'Library/analysis/records', `${settled.quality_check.analysis_id}.json`)));
    assert.equal(record.analysis_kind, 'capture_quality');
    assert.equal(record.inputs[0].revision_id, settled.revision_id);
    const provenance = JSON.parse(record.body_markdown);
    assert.equal(provenance.state, quality);
    assert.equal(provenance.model.model_id, 'synthetic-capture-quality');
    assert(provenance.prompt.sha256.startsWith('sha256:'));
    if (quality === 'corrected') assert.notEqual(provenance.pre_draft_sha256, provenance.post_draft_sha256);
    else assert.equal(provenance.pre_draft_sha256, provenance.post_draft_sha256);
  }
  assert(sawRunning, 'real pending model check must be observable');
  for (const [host, state] of [['challenge', 'needs_user'], ['rate', 'retry_wait']]) {
    const job = await post('url-jobs/enqueue', { request_key: `chrome_capture_${randomBytes(24).toString('base64url')}`,
      url: `https://${host}.example.invalid/article`, include_comments: true, quality_check: 'configured' });
    const result = await settle(job.body);
    assert.equal(result.state, state);
    assert.equal(result.quality_check.state, 'pending');
    assert.equal(result.quality_check.reason, 'awaiting_capture');
  }
  const html = '<article><h1>Snapshot quality</h1><p>Harmless synthetic snapshot body to check formatting without any comments or personal information.</p></article>';
  const snapshot = { external_capture_id: 'synthetic-snapshot-quality', captured_at: '2026-11-01T12:00:00Z',
    source_url: 'https://snapshot.example.invalid/article', sensitivity: 'internal', quality_check: 'configured',
    html: { media_type: 'text/html', byte_size: Buffer.byteLength(html), sha256: createHash('sha256').update(html).digest('hex'), body: html } };
  const first = await post('captures', snapshot);
  assert.equal(first.status, 201);
  assert.equal(first.body.quality_check.state, 'passed');
  assert.equal(first.body.comments.state, 'not_requested');
  const replay = await post('captures', snapshot);
  assert.equal(replay.status, 200);
  assert.equal(replay.body.item_id, first.body.item_id);
  assert.equal(replay.body.quality_check.analysis_id, first.body.quality_check.analysis_id);
  assert.equal((await post('captures', { ...snapshot, quality_check: 'off' })).body.error, 'request_identity_conflict');
  const { quality_check, ...omitted } = snapshot;
  assert.equal((await post('captures', omitted)).body.error, 'request_identity_conflict');
  for (const binary of [false, true]) {
    async function capture(payload) {
      if (!binary) return post('captures', payload);
      const { body, ...descriptor } = payload.html;
      const manifest = Buffer.from(JSON.stringify({ schema_version: 1, ...payload, html: descriptor }));
      const length = Buffer.alloc(4); length.writeUInt32BE(manifest.length);
      const response = await fetch(new URL('api/v1/captures', fixture.baseUrl), {
        method: 'POST', headers: { Origin: fixture.origin, 'Content-Type': 'application/vnd.memex.capture-v1',
          'X-MemeX-Intake-Secret': fixture.secret, 'X-MemeX-Intake-Nonce': randomBytes(32).toString('hex') },
        body: Buffer.concat([Buffer.from('MEMEX\0\0\x01'), length, manifest, Buffer.from(body)]),
      });
      return { status: response.status, body: await response.json() };
    }
    for (const explicit of [false, true]) {
      const payload = { ...omitted, external_capture_id: `off-mode-${binary}-${explicit}` };
      const original = await capture(explicit ? { ...payload, quality_check: 'off' } : payload);
      assert.equal(original.status, 201);
      const replay = await capture(explicit ? payload : { ...payload, quality_check: 'off' });
      assert.equal(replay.status, 200);
      assert.equal(replay.body.item_id, original.body.item_id);
      assert.equal((await capture({ ...payload, quality_check: 'configured' })).body.error, 'request_identity_conflict');
    }
    const configured = { ...snapshot, external_capture_id: `configured-mode-${binary}` };
    assert.equal((await capture(configured)).status, 201);
    delete configured.quality_check;
    assert.equal((await capture(configured)).body.error, 'request_identity_conflict');
  }
});
