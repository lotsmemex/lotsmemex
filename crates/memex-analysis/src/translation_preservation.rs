//! Cost-free preservation, deliberately separate from authorized job migration.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslationPreservation {
    Preserved(TranslationAnalysisPair),
    /// The caller must not report preservation success or authorize generation.
    Gap {
        reason: &'static str,
    },
}

/// Rebind a complete ordinary-article translation to a metadata-only revision.
/// No provider is accepted, no queued/running/authorized job is created, and old
/// jobs/records/reading state are untouched. Original generation time and model
/// provenance are retained; the new job has zero attempts and no authorization.
/// Missing/incomplete source work returns Gap; integrity/IO failures return Err
/// (also a preservation failure). Summary and book chapters are not handled.
pub fn preserve_completed_translation(
    library: impl AsRef<Path>,
    item_id: &str,
    previous_revision_id: &str,
    current_revision_id: &str,
) -> Result<TranslationPreservation> {
    memex_store::with_consistent_snapshot(library, |root| {
        preserve_locked(root, item_id, previous_revision_id, current_revision_id)
    })
    .context("translation_preservation_failed")
}

fn preserve_locked(
    root: &Path,
    item: &str,
    previous: &str,
    current: &str,
) -> Result<TranslationPreservation> {
    let gap = |reason| Ok(TranslationPreservation::Gap { reason });
    let (header, revision) = memex_store::read_item(root, item)?;
    if revision.revision_id != current || previous == current {
        return gap("revision_mismatch");
    }
    if !matches!(
        header.item_type.as_str(),
        "article" | "journal_article" | "webpage"
    ) {
        return gap("unsupported_item_type");
    }
    // Store's public historical-text API verifies the FULL current chain and
    // proves this exact ancestor is reachable (also rejects book body targets).
    // Only then load that already-verified revision, under the same Store lock.
    let historical = memex_store::reader::revision_text::chunk(
        root,
        item,
        previous,
        memex_store::reader::revision_text::Target::Body {},
        0,
        8192,
        "any",
        revision.sensitivity,
    )?;
    let old: ItemRevision = serde_json::from_slice(&crate::read_bounded_regular(
        &root
            .join("items")
            .join(item)
            .join("revisions")
            .join(format!("{previous}.json")),
        16 * 1024 * 1024,
        "verified historical revision",
    )?)?;
    if old.item_id != item
        || old.revision_id != previous
        || old.title != revision.title
        || old.content_markdown != revision.content_markdown
        || old.evidence_ids != revision.evidence_ids
        || old.attachments != revision.attachments
        || old.sensitivity != revision.sensitivity
        || historical.sha256 != memex_store::reader::reader_sha256(old.content_markdown.as_bytes())
    {
        return gap("not_metadata_only");
    }
    let mut queue = TranslationQueue::open(root)?;
    // Serializes concurrent preservation and queue writers, without exposing a
    // runnable intermediate state. Analysis publication is idempotent on crash.
    let tx = queue
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let binding = tx
        .query_row(
            "SELECT job_id,item_id,revision_id,source_language,target_language,state,
                reason_code,attempt_count,created_at,updated_at,revision_sha256,
                title_analysis_id,document_analysis_id,model_id,model_sha256,
                adapter_id,adapter_version,request_timeout_ms
         FROM translation_jobs WHERE item_id=?1 AND revision_id=?2 AND target_language=?3",
            params![item, previous, TARGET_LANGUAGE],
            |row| {
                Ok((
                    job_view_columns(row)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, Option<String>>(16)?,
                    row.get::<_, Option<i64>>(17)?,
                ))
            },
        )
        .optional()?;
    let Some((view, hash, title_id, document_id, model_id, model_hash, adapter, version, timeout)) =
        binding
    else {
        return gap("source_job_missing");
    };
    if view.state != TranslationJobState::Succeeded {
        return gap("source_not_succeeded");
    }
    let (
        Some(title_id),
        Some(document_id),
        Some(model_id),
        Some(model_sha256),
        Some(adapter_id),
        Some(adapter_version),
        Some(ms),
    ) = (
        title_id,
        document_id,
        model_id,
        model_hash,
        adapter,
        version,
        timeout,
    )
    else {
        return gap("source_evidence_or_model_missing");
    };
    let model = TranslationModelDescriptor {
        model_id,
        model_sha256,
        adapter_id,
        adapter_version,
        request_timeout: Duration::from_millis(ms.max(0) as u64),
    };
    if !model.is_valid() {
        return gap("source_model_invalid");
    }
    if hash != revision_ref(item, &old)?.revision_sha256 {
        return gap("source_revision_mismatch");
    }
    let title = crate::read_record(root, &title_id)?;
    let document = crate::read_record(root, &document_id)?;
    let mut job = TranslationJobRecord {
        view,
        revision_sha256: hash,
        lease_owner: String::new(),
        translated_title: None,
        translated_document: None,
        model: Some(model.clone()),
    };
    for (record, purpose, source) in [
        (
            &title,
            GeneratedTextPurpose::TitleTranslation,
            old.title.as_str(),
        ),
        (
            &document,
            GeneratedTextPurpose::DocumentTranslation,
            old.content_markdown.as_str(),
        ),
    ] {
        if !valid_translation_output(&record.body_markdown, source, purpose)
            || *record
                != translation_record_at(
                    root,
                    &job,
                    &model,
                    purpose,
                    &record.body_markdown,
                    Some(&old),
                )?
        {
            return gap("source_analysis_mismatch");
        }
    }
    if competing_translation_records(root, &job, &title, &document)? {
        return gap("source_analysis_conflict");
    }
    job.view.revision_id = current.into();
    job.view.job_id = format!(
        "translation_{:x}",
        Sha256::digest(
            format!(
                "{item}\0{current}\0{}\0{TARGET_LANGUAGE}",
                job.view.source_language
            )
            .as_bytes()
        )
    );
    job.revision_sha256 = revision_ref(item, &revision)?.revision_sha256;
    let title = translation_record(
        root,
        &job,
        &model,
        GeneratedTextPurpose::TitleTranslation,
        &title.body_markdown,
    )?;
    let document = translation_record(
        root,
        &job,
        &model,
        GeneratedTextPurpose::DocumentTranslation,
        &document.body_markdown,
    )?;
    if competing_translation_records(root, &job, &title, &document)? {
        return gap("target_analysis_conflict");
    }
    let target_count: i64 = tx.query_row("SELECT COUNT(*) FROM translation_jobs WHERE (item_id=?1 AND revision_id=?2 AND target_language=?3) OR job_id=?4",
        params![item, current, TARGET_LANGUAGE, job.view.job_id], |row| row.get(0))?;
    let exists = target_count != 0;
    // A reconciler may enqueue the new revision before preservation runs. Only
    // its pristine, identity-exact placeholder can be completed in place. This
    // predicate and the eventual upsert share the IMMEDIATE writer transaction.
    // Batch cache rows are content-bound, not target-job progress: leave them
    // alone, including caches shared with the old successfully translated text.
    let placeholder = target_count == 1 && tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM translation_jobs
          WHERE job_id=?1 AND item_id=?2 AND revision_id=?3 AND revision_sha256=?4
            AND source_language=?5 AND target_language=?6 AND classifier_version=?7
            AND state='queued' AND provider_authorized=0 AND attempt_count=0
            AND reason_code IS NULL AND lease_owner IS NULL AND lease_until IS NULL
            AND translated_title IS NULL AND translated_document IS NULL
            AND title_analysis_id IS NULL AND document_analysis_id IS NULL
            AND batches_done IS NULL AND batches_total IS NULL
            AND model_id IS NULL AND model_sha256 IS NULL AND adapter_id IS NULL
            AND adapter_version IS NULL AND request_timeout_ms IS NULL)",
        params![job.view.job_id, item, current, job.revision_sha256,
            job.view.source_language, TARGET_LANGUAGE, CLASSIFIER_VERSION],
        |row| row.get::<_, bool>(0),
    )?;
    if exists && !placeholder {
        let expected = TranslationAnalysisPair {
            title_analysis_id: title.analysis_id.clone(),
            document_analysis_id: document.analysis_id.clone(),
        };
        // Compare persisted job identities, not the repaired document's display overlay.
        if base_effective_translation(root, item, current)?.map(|(pair, _)| pair) != Some(expected.clone()) {
            return gap("target_job_conflict");
        }
        // Rebuild missing portable projections on replay, too.
    }
    let (a, b) = crate::apply_record_pair(root, &title, &document)?;
    if !a.warning_codes.is_empty() || !b.warning_codes.is_empty() {
        return gap("analysis_projection_pending");
    }
    if !exists || placeholder {
        let changed = tx.execute(
            "INSERT INTO translation_jobs(job_id,item_id,revision_id,revision_sha256,
            source_language,target_language,classifier_version,state,created_at,updated_at,
            model_id,model_sha256,adapter_id,adapter_version,request_timeout_ms,
            title_analysis_id,document_analysis_id,provider_authorized)
            VALUES(?1,?2,?3,?4,?5,?6,?7,'succeeded',?8,?9,?10,?11,?12,?13,?14,?15,?16,0)
            ON CONFLICT(job_id) DO UPDATE SET
                state=excluded.state,created_at=excluded.created_at,updated_at=excluded.updated_at,
                model_id=excluded.model_id,model_sha256=excluded.model_sha256,
                adapter_id=excluded.adapter_id,adapter_version=excluded.adapter_version,
                request_timeout_ms=excluded.request_timeout_ms,
                title_analysis_id=excluded.title_analysis_id,
                document_analysis_id=excluded.document_analysis_id
            WHERE ?17",
            params![
                job.view.job_id,
                item,
                current,
                job.revision_sha256,
                job.view.source_language,
                TARGET_LANGUAGE,
                CLASSIFIER_VERSION,
                job.view.created_at,
                Utc::now().to_rfc3339(),
                model.model_id,
                model.model_sha256,
                model.adapter_id,
                model.adapter_version,
                ms,
                title.analysis_id,
                document.analysis_id,
                placeholder
            ],
        )?;
        if changed != 1 {
            return gap("target_job_conflict");
        }
    }
    tx.commit()?;
    Ok(TranslationPreservation::Preserved(
        TranslationAnalysisPair {
            title_analysis_id: title.analysis_id,
            document_analysis_id: document.analysis_id,
        },
    ))
}
