//! Table-only translation repair. The original job and its immutable records stay untouched.
use super::*;
use std::ops::Range;

/// Byte ranges of pipe tables outside fenced code. Blank lines and surrounding prose are
/// deliberately excluded: replacing these ranges cannot reformat the rest of a translation.
pub fn translation_table_ranges(markdown: &str) -> Vec<Range<usize>> {
    use pulldown_cmark::{Event, Options, Parser, Tag};
    Parser::new_ext(markdown, Options::ENABLE_TABLES)
        .into_offset_iter()
        .filter_map(|(event, range)| {
            if !matches!(event, Event::Start(Tag::Table(_))) {
                return None;
            }
            let end = range.start + markdown[range.clone()].trim_end_matches(['\r', '\n']).len();
            Some(range.start..end)
        })
        .collect()
}

fn table_cells(line: &str) -> Vec<&str> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    // An escaped trailing pipe belongs to the cell, not the table border.
    let line = if line.ends_with('|')
        && line[..line.len() - 1]
            .bytes()
            .rev()
            .take_while(|b| *b == b'\\')
            .count()
            % 2
            == 0
    {
        &line[..line.len() - 1]
    } else {
        line
    };
    let mut cells = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if ch == '|' && !escaped {
            cells.push(line[start..index].trim());
            start = index + 1;
        }
        escaped = ch == '\\' && !escaped;
    }
    cells.push(line[start..].trim());
    cells
}

fn protected_cell_parts(cell: &str) -> (Vec<&str>, Vec<&str>, Vec<&str>) {
    let mut code = Vec::new();
    let mut links = Vec::new();
    let mut numbers = Vec::new();
    let mut rest = cell;
    while let Some(index) = rest.find('`') {
        rest = &rest[index..];
        let count = rest.bytes().take_while(|b| *b == b'`').count();
        let delimiter = &rest[..count];
        let Some(end) = rest[count..].find(delimiter) else {
            break;
        };
        code.push(&rest[..count + end + count]);
        rest = &rest[count + end + count..];
    }
    rest = cell;
    while let Some(index) = rest.find("](") {
        rest = &rest[index + 2..];
        let mut depth = 1;
        let mut end = None;
        for (index, ch) in rest.char_indices() {
            if ch == '(' {
                depth += 1;
            }
            if ch == ')' {
                depth -= 1;
            }
            if depth == 0 {
                end = Some(index);
                break;
            }
        }
        let Some(end) = end else { break };
        links.push(&rest[..end]);
        rest = &rest[end + 1..];
    }
    let bytes = cell.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += cell[index..].chars().next().unwrap().len_utf8();
            continue;
        }
        let mut start = index;
        // A leading decimal point belongs to .75, unlike sentence-final punctuation.
        if cell[..start].ends_with('.')
            && !cell[..start - 1].chars().next_back().is_some_and(|c| c.is_ascii_digit() || c == '.')
        {
            start -= 1;
        }
        start -= cell[..start].chars().rev()
            .take_while(|c| matches!(c, '-' | '+' | '−' | '$' | '€' | '£' | '¥'))
            .map(char::len_utf8).sum::<usize>();
        let mut end = index + 1;
        while end < bytes.len() {
            if bytes[end].is_ascii_digit() {
                end += 1;
            } else if matches!(bytes[end], b'.' | b',' | b'/' | b':')
                && bytes[end - 1].is_ascii_digit()
                && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
            {
                // Decimal/grouping/ratio/time separators are internal, not trailing.
                end += 1;
            } else {
                if bytes[end] == b'%' { end += 1; }
                break;
            }
        }
        numbers.push(&cell[start..end]);
        index = end;
    }
    (code, links, numbers)
}

fn cell_prose(cell: &str) -> String {
    let (code, links, numbers) = protected_cell_parts(cell);
    let mut prose = cell.to_owned();
    for part in code.into_iter().chain(links).chain(numbers) {
        prose = prose.replace(part, "");
    }
    prose
}

/// A translated table must preserve cells, row order, alignment, code, link destinations and
/// numeric values. A prose response or a dropped/merged column is never installed.
pub fn translated_table_failure(translated: &str, source: &str) -> Option<&'static str> {
    let source = source.trim();
    let translated = translated.trim();
    if translation_table_ranges(source) != [0..source.len()]
        || translation_table_ranges(translated) != [0..translated.len()]
    {
        return Some("table_shape");
    }
    let source_rows: Vec<_> = source.lines().map(table_cells).collect();
    let target_rows: Vec<_> = translated.lines().map(table_cells).collect();
    if source_rows.len() != target_rows.len() {
        return Some("table_rows");
    }
    let (mut source_prose, mut target_prose) = (String::new(), String::new());
    for (index, (source, target)) in source_rows.iter().zip(&target_rows).enumerate() {
        if source.len() != target.len() {
            return Some("table_columns");
        }
        for (source, target) in source.iter().zip(target) {
            if index == 1
                && (source.starts_with(':') != target.starts_with(':')
                    || source.ends_with(':') != target.ends_with(':'))
            {
                return Some("table_alignment");
            }
            if protected_cell_parts(source) != protected_cell_parts(target) {
                return Some("table_values");
            }
            let (a, b) = (cell_prose(source), cell_prose(target));
            if source.is_empty() != target.is_empty()
                || !a.trim().is_empty() && translated_block_failure(&b, &a).is_some()
            {
                return Some("table_cell_untranslated");
            }
            source_prose.push_str(&a);
            source_prose.push(' ');
            target_prose.push_str(&b);
            target_prose.push(' ');
        }
    }
    translated_block_failure(&target_prose, &source_prose)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationTablePlan {
    pub repair_id: String,
    pub item_id: String,
    pub revision_id: String,
    pub source_revision_id: String,
    pub base_analysis_id: String,
    pub tables: usize,
}

#[derive(Debug, Serialize)]
pub struct TranslationTableRepairReceipt {
    pub plan: TranslationTablePlan,
    pub state: String,
    pub completed_tables: usize,
    /// Counts dispatch intents, including a call whose outcome may be unknown. The provider
    /// adapter does not expose billed usage: this is not a token count or a price quote.
    pub requests_started: usize,
    pub model_id: Option<String>,
    pub document_analysis_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_check: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_output_sha256: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct TableAttempt {
    request: usize,
    table_index: usize,
    state: String,
    check: Option<String>,
    // Real derived content: local owner-only database only, never a public receipt/log.
    output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reused: Option<bool>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Repair {
    plan: TranslationTablePlan,
    model: TranslationModelDescriptor,
    created_at: String,
    state: String,
    outputs: Vec<String>,
    requests_started: usize,
    document_analysis_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attempts: Vec<TableAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    continuation_limit: Option<usize>,
}

struct RepairInput {
    plan: TranslationTablePlan,
    source: ItemRevision,
    base: crate::AnalysisRecord,
    ranges: Vec<Range<usize>>,
}

fn repair_id(item_id: &str, source_revision: &str, base_analysis: &str) -> Result<String> {
    // Metadata revisions cannot generate a new paid identity for the same source/base.
    let identity =
        serde_json::to_vec(&("table-repair-v1", item_id, source_revision, base_analysis))?;
    Ok(format!("table_repair_{:x}", Sha256::digest(identity)))
}

fn repair_input(root: &Path, item_id: &str, expected_analysis: &str) -> Result<RepairInput> {
    let (_, current) = memex_store::read_item(root, item_id)?;
    let (pair, source) = base_effective_translation(root, item_id, &current.revision_id)?
        .context("translation_unavailable")?;
    if pair.document_analysis_id != expected_analysis {
        bail!("stale_translation_analysis");
    }
    repair_input_for_pair(root, item_id, &current.revision_id, &pair, source)
}

fn repair_input_for_pair(
    root: &Path,
    item_id: &str,
    current_revision: &str,
    pair: &TranslationAnalysisPair,
    source: ItemRevision,
) -> Result<RepairInput> {
    let expected_analysis = &pair.document_analysis_id;
    let base = crate::read_record(root, expected_analysis)?;
    let original = translation_table_ranges(&source.content_markdown);
    let translated = translation_table_ranges(&base.body_markdown);
    if original.len() != translated.len() {
        bail!("translation_table_mapping_changed");
    }
    let mut ranges = Vec::new();
    for (source_range, target_range) in original.into_iter().zip(translated) {
        let source_table = &source.content_markdown[source_range];
        let translated_table = &base.body_markdown[target_range.clone()];
        if source_table == translated_table {
            // One provider call per table: no glossary or segmented request can escape the
            // durable per-table dispatch intent. Larger tables need an explicit new contract.
            if source_table.len() > 12 * 1024 {
                bail!("translation_table_too_large");
            }
            ranges.push(target_range);
        }
    }
    let plan = TranslationTablePlan {
        repair_id: repair_id(item_id, &source.revision_id, expected_analysis)?,
        item_id: item_id.into(),
        revision_id: current_revision.into(),
        source_revision_id: source.revision_id.clone(),
        base_analysis_id: expected_analysis.clone(),
        tables: ranges.len(),
    };
    Ok(RepairInput {
        plan,
        source,
        base,
        ranges,
    })
}

fn same_repair_binding(left: &TranslationTablePlan, right: &TranslationTablePlan) -> bool {
    left.repair_id == right.repair_id
        && left.item_id == right.item_id
        && left.source_revision_id == right.source_revision_id
        && left.base_analysis_id == right.base_analysis_id
        && left.tables == right.tables
}

fn read_repair(connection: &Connection, id: &str) -> Result<Option<Repair>> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='translation_table_repairs')",
        [], |row| row.get(0))?;
    if !exists {
        return Ok(None);
    }
    let json: Option<String> = connection
        .query_row(
            "SELECT record_json FROM translation_table_repairs WHERE repair_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    json.map(|json| serde_json::from_str(&json).map_err(Into::into))
        .transpose()
}

fn receipt(plan: TranslationTablePlan, repair: Option<&Repair>) -> TranslationTableRepairReceipt {
    TranslationTableRepairReceipt {
        state: repair.map_or("planned", |r| r.state.as_str()).into(),
        completed_tables: repair.map_or(0, |r| r.outputs.len()),
        requests_started: repair.map_or(0, |r| r.requests_started),
        model_id: repair.map(|r| r.model.model_id.clone()),
        document_analysis_id: repair.and_then(|r| r.document_analysis_id.clone()),
        last_check: repair
            .and_then(|r| r.attempts.last())
            .and_then(|a| a.check.clone()),
        last_output_sha256: repair
            .and_then(|r| r.attempts.last())
            .and_then(|a| a.output.as_ref())
            .map(|s| format!("{:x}", Sha256::digest(s.as_bytes()))),
        plan,
    }
}

/// Read-only planning/status, without credentials or model access. Repeating it after an
/// uncertain CLI exit reveals the exact same request identity, never creates another job.
pub fn translation_table_repair_plan(
    root: &Path,
    item_id: &str,
    expected_analysis: &str,
) -> Result<TranslationTableRepairReceipt> {
    memex_store::with_consistent_snapshot(root, |root| {
        let input = repair_input(root, item_id, expected_analysis)?;
        let repair = match open_status_database(root)? {
            Some((_, connection)) => {
                if let Some((repair, bound)) = bound_repair(root, &connection, &input)? {
                    if repair.state == "succeeded" {
                        verified_assembly(root, &bound, &repair)?;
                    }
                    return Ok(receipt(repair.plan.clone(), Some(&repair)));
                }
                read_repair(&connection, &input.plan.repair_id)?
            }
            None => None,
        };
        Ok(receipt(input.plan, repair.as_ref()))
    })
}

fn replace_repair(connection: &Connection, before: &Repair, after: &Repair) -> Result<()> {
    if connection.execute(
        "UPDATE translation_table_repairs SET record_json=?1 WHERE repair_id=?2 AND record_json=?3",
        params![
            serde_json::to_string(after)?,
            before.plan.repair_id,
            serde_json::to_string(before)?
        ],
    )? != 1
    {
        bail!("translation_table_repair_conflict");
    }
    Ok(())
}

/// One explicit continuation of a known-invalid answer, never unknown/in-flight. The original
/// paid identity and failure remain, with at most two more intents and three total. New known
/// answers are kept privately so a corrected validator can reuse them without another call.
pub fn resume_invalid_table_repair(
    root: &Path,
    item: &str,
    expected: &str,
    repair_id: &str,
) -> Result<TranslationTableRepairReceipt> {
    let mut queue = TranslationQueue::open(root)?;
    memex_store::with_consistent_snapshot(root, |root| {
        let tx = queue
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = repair_input(root, item, expected)?;
        let (before, bound) = bound_repair(root, &tx, &current)?.context("missing_table_repair")?;
        if before.plan.repair_id != repair_id
            || before.state != "invalid_output"
            || before.continuation_limit.is_some()
            || !(1..3).contains(&before.requests_started)
        {
            bail!("table_repair_continuation_not_permitted");
        }
        let mut after = before.clone();
        after.continuation_limit = Some(3);
        if after.attempts.last().map(|a| a.request) != Some(before.requests_started) {
            after.attempts.push(TableAttempt {
                request: before.requests_started,
                table_index: before.outputs.len(),
                state: "invalid_output".into(),
                check: Some("legacy_response_not_retained".into()),
                output: None,
                reused: None,
            });
        }
        let attempt = after.attempts.last_mut().context("missing_table_attempt")?;
        if let Some(output) = attempt
            .output
            .as_ref()
            .filter(|output| output.len() <= MAX_TRANSLATION_OUTPUT_BYTES
                && attempt.check.as_deref() != Some("incomplete_length"))
        {
            let table = &bound.base.body_markdown[bound
                .ranges
                .get(before.outputs.len())
                .context("invalid_table_index")?
                .clone()];
            if translated_table_failure(output, table).is_none() {
                after.outputs.push(output.trim().to_owned());
                attempt.reused = Some(true);
            }
        }
        after.state = "ready".into();
        replace_repair(&tx, &before, &after)?;
        tx.commit()?;
        Ok(receipt(after.plan.clone(), Some(&after)))
    })
}

/// Revalidate one retained known-invalid response under the already-consumed grant. This
/// never resets the budget, dispatches a model, or treats a partial/unknown answer as complete.
pub fn revalidate_cached_table_repair(
    root: &Path,
    item: &str,
    expected: &str,
    repair_id: &str,
    expected_response_sha256: &str,
) -> Result<TranslationTableRepairReceipt> {
    let mut queue = TranslationQueue::open(root)?;
    memex_store::with_consistent_snapshot(root, |root| {
        let tx = queue.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = repair_input(root, item, expected)?;
        let (before, bound) = bound_repair(root, &tx, &current)?.context("missing_table_repair")?;
        if before.plan.repair_id != repair_id || before.state != "invalid_output"
            || before.continuation_limit != Some(3) || before.requests_started != 2
        {
            bail!("table_repair_cached_revalidation_not_permitted");
        }
        let attempt = before.attempts.last().context("missing_known_table_response")?;
        if attempt.request != before.requests_started || attempt.table_index != before.outputs.len()
            || attempt.state != "invalid_output" || attempt.check.as_deref() == Some("incomplete_length")
            || attempt.reused == Some(true)
        {
            bail!("table_repair_cached_revalidation_not_permitted");
        }
        let output = attempt.output.as_ref().context("missing_known_table_response")?;
        let table = &bound.base.body_markdown[bound.ranges.get(before.outputs.len()).context("invalid_table_index")?.clone()];
        if output.len() > MAX_TRANSLATION_OUTPUT_BYTES
            || format!("{:x}", Sha256::digest(output.as_bytes())) != expected_response_sha256
            || translated_table_failure(output, table).is_some()
        {
            bail!("table_repair_cached_response_not_accepted");
        }
        let mut after = before.clone();
        after.outputs.push(output.trim().to_owned());
        // Keep the original invalid state/check/output in attempt history; record only reuse.
        after.attempts.last_mut().unwrap().reused = Some(true);
        after.state = "ready".into();
        replace_repair(&tx, &before, &after)?;
        tx.commit()?;
        Ok(receipt(after.plan.clone(), Some(&after)))
    })
}

fn repaired_record(input: &RepairInput, repair: &Repair) -> Result<crate::AnalysisRecord> {
    if !same_repair_binding(&input.plan, &repair.plan)
        || !repair.model.is_valid()
        || repair.outputs.len() != input.ranges.len()
    {
        bail!("invalid_translation_table_repair");
    }
    let mut body = input.base.body_markdown.clone();
    for (range, output) in input.ranges.iter().zip(&repair.outputs).rev() {
        if translated_table_failure(output, &input.base.body_markdown[range.clone()]).is_some()
            || output.len() > MAX_TRANSLATION_OUTPUT_BYTES
        {
            bail!("invalid_translation_table_output");
        }
        body.replace_range(range.clone(), output);
    }
    let mut record = input.base.clone();
    record.body_markdown = body;
    record.title = "Chinese document translation with repaired tables".into();
    record.created_at = repair.created_at.clone();
    // This is an assembly of the immutable base and separately generated tables, not a fresh
    // whole-document generation. The repair row binds model + outputs; the base retains its
    // original generated_text. Old daemons ignore this assembly when validating old jobs.
    record.generated_text = None;
    record.provenance = crate::AnalysisProvenance {
        actor_kind: ActorKind::Tool,
        actor_id: "memex-table-repair".into(),
        tool_id: "memex-table-repair".into(),
        tool_version: "1".into(),
        config_digest: format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(&(
                &repair.plan,
                &repair.model,
                &repair.outputs
            ))?)
        ),
    };
    record.analysis_id = crate::analysis_id(&record)?;
    crate::validate_record(&record)?;
    Ok(record)
}

/// Explicit paid execution for exactly this item/base. Calling intent commits before network
/// dispatch. A timeout, crash or interrupted response can never automatically dispatch again.
pub fn repair_translation_tables(
    root: &Path,
    item_id: &str,
    expected_analysis: &str,
    model: &mut dyn TranslationModel,
) -> Result<TranslationTableRepairReceipt> {
    let status = translation_table_repair_plan(root, item_id, expected_analysis)?;
    if !matches!(status.state.as_str(), "planned" | "ready") {
        return Ok(status);
    }
    let input = memex_store::with_consistent_snapshot(root, |root| {
        repair_input(root, item_id, expected_analysis)
    })?;
    if input.plan.tables == 0 {
        return Ok(receipt(input.plan, None));
    }
    let mut queue = TranslationQueue::open(root)?;
    queue.connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS translation_table_repairs(
        repair_id TEXT PRIMARY KEY,record_json TEXT NOT NULL)",
    )?;
    let descriptor = model.descriptor();
    if !descriptor.is_valid() {
        bail!("invalid_translation_table_model");
    }
    let input = memex_store::with_consistent_snapshot(root, |root| {
        let tx = queue
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let fresh = repair_input(root, item_id, expected_analysis)?;
        let bound = if let Some((_, bound)) = bound_repair(root, &tx, &fresh)? {
            bound
        } else {
            let initial = Repair {
                plan: fresh.plan.clone(),
                model: descriptor.clone(),
                created_at: Utc::now().to_rfc3339(),
                state: "ready".into(),
                outputs: Vec::new(),
                requests_started: 0,
                document_analysis_id: None,
                attempts: Vec::new(),
                continuation_limit: None,
            };
            tx.execute(
                "INSERT INTO translation_table_repairs VALUES(?1,?2)",
                params![fresh.plan.repair_id, serde_json::to_string(&initial)?],
            )?;
            fresh
        };
        tx.commit()?;
        Ok(bound)
    })?;
    loop {
        let before = read_repair(&queue.connection, &input.plan.repair_id)?
            .context("missing_table_repair")?;
        if !same_repair_binding(&before.plan, &input.plan) {
            bail!("translation_table_repair_conflict");
        }
        if before.state != "ready" {
            return Ok(receipt(input.plan, Some(&before)));
        }
        if !same_model(&before.model, &descriptor) {
            bail!("translation_table_model_changed");
        }
        if before.outputs.len() == input.ranges.len() {
            // Canonical item edits are excluded by the store snapshot; an immediate database
            // transaction excludes job/pointer changes while validating and publishing.
            return memex_store::with_consistent_snapshot(root, |root| {
                let tx = queue
                    .connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                let current = repair_input(root, item_id, expected_analysis)?;
                let (_, fresh) =
                    bound_repair(root, &tx, &current)?.context("missing_table_repair")?;
                if fresh.plan != input.plan {
                    bail!("stale_translation_table_plan");
                }
                let record = repaired_record(&fresh, &before)?;
                crate::apply_record(root, &record)?;
                let mut after = before.clone();
                after.state = "succeeded".into();
                after.document_analysis_id = Some(record.analysis_id);
                replace_repair(&tx, &before, &after)?;
                tx.commit()?;
                Ok(receipt(input.plan.clone(), Some(&after)))
            });
        }
        if before.outputs.len() > input.ranges.len() {
            bail!("invalid_translation_table_repair");
        }
        let mut calling = before.clone();
        if before
            .continuation_limit
            .is_some_and(|limit| before.requests_started >= limit)
        {
            calling.state = "budget_exhausted".into();
            replace_repair(&queue.connection, &before, &calling)?;
            return Ok(receipt(calling.plan.clone(), Some(&calling)));
        }
        calling.state = "in_flight_or_unknown".into();
        calling.requests_started += 1;
        calling.attempts.push(TableAttempt {
            request: calling.requests_started,
            table_index: before.outputs.len(),
            state: "in_flight_or_unknown".into(),
            check: None,
            output: None,
            reused: None,
        });
        memex_store::with_consistent_snapshot(root, |root| {
            let tx = queue
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = repair_input(root, item_id, expected_analysis)?;
            let (_, fresh) = bound_repair(root, &tx, &current)?.context("missing_table_repair")?;
            if fresh.plan != input.plan {
                bail!("stale_translation_table_plan");
            }
            replace_repair(&tx, &before, &calling)?;
            tx.commit()?;
            Ok(())
        })?;
        let table = &input.base.body_markdown[input.ranges[before.outputs.len()].clone()];
        let (_, prompt, _) = generated_text_prompt(GeneratedTextPurpose::DocumentTranslation);
        let source_language = input
            .base
            .generated_text
            .as_ref()
            .context("missing_translation_descriptor")?
            .source_language
            .as_str();
        let response = model.translate_table(TranslationModelInput {
            purpose: GeneratedTextPurpose::DocumentTranslation,
            source_language,
            target_language: TARGET_LANGUAGE,
            prompt,
            source_text: table,
            document_title: &input.source.title,
            document_markdown: table,
            progress: None,
        });
        let mut after = calling.clone();
        let attempt = after.attempts.last_mut().context("missing_table_attempt")?;
        if let Ok(result) = &response {
            let (output, incomplete) = match result {
                TableTranslationOutput::Complete(output) => (output, false),
                TableTranslationOutput::IncompleteLength(output) => (output, true),
            };
            attempt.check = if incomplete {
                Some("incomplete_length".into())
            } else if output.len() > MAX_TRANSLATION_OUTPUT_BYTES {
                Some("too_large".into())
            } else {
                translated_table_failure(output, table).map(str::to_owned)
            };
            if output.len() <= MAX_TRANSLATION_OUTPUT_BYTES {
                attempt.output = Some(output.clone());
            }
        } else {
            attempt.check = Some(
                match &response {
                    Err(TranslationModelError::OutcomeUnknown) => "provider_outcome_unknown",
                    Err(TranslationModelError::Timeout) => "provider_timeout",
                    _ => "provider_error",
                }
                .into(),
            );
        }
        match response {
            Ok(TableTranslationOutput::Complete(output))
                if output.len() <= MAX_TRANSLATION_OUTPUT_BYTES
                    && translated_table_failure(&output, table).is_none() =>
            {
                after.outputs.push(output.trim().to_owned());
                after.state = "ready".into();
            }
            Ok(_) => after.state = "invalid_output".into(),
            Err(TranslationModelError::OutcomeUnknown | TranslationModelError::Timeout) => {
                after.state = "outcome_unknown".into()
            }
            Err(_) => after.state = "provider_failed".into(),
        }
        after
            .attempts
            .last_mut()
            .context("missing_table_attempt")?
            .state = after.state.clone();
        replace_repair(&queue.connection, &calling, &after)?;
    }
}

pub(super) fn apply_table_repair_projection(
    root: &Path,
    item_id: &str,
    revision_id: &str,
    pair: &mut TranslationAnalysisPair,
    source: &ItemRevision,
) -> Result<()> {
    let Some((_, connection)) = open_status_database(root)? else {
        return Ok(());
    };
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='translation_table_repairs')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(());
    }
    let has_repairs: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM translation_table_repairs WHERE json_extract(record_json,'$.plan.item_id')=?1)", [item_id], |r| r.get(0))?;
    if !has_repairs {
        return Ok(());
    }
    let input = repair_input_for_pair(root, item_id, revision_id, pair, source.clone())?;
    // Historical reads never apply an overlay for a different canonical revision.
    if memex_store::read_item(root, item_id)?.1.revision_id != revision_id {
        return Ok(());
    }
    if let Some((repair, bound)) = bound_repair(root, &connection, &input)? {
        if repair.state == "succeeded" {
            pair.document_analysis_id = verified_assembly(root, &bound, &repair)?.analysis_id;
        }
    }
    Ok(())
}

fn bound_repair(
    root: &Path,
    connection: &Connection,
    input: &RepairInput,
) -> Result<Option<(Repair, RepairInput)>> {
    let mut candidates = vec![(input.plan.clone(), input.source.clone(), input.base.clone())];
    // Cost-free preservation can rebind the same base bytes to a new metadata revision/job.
    // Reuse only a verified ancestor with identical translated prose, never a merely similar
    // document or another item's repair. New title records remain the current job's records.
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='translation_table_repairs')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    for source in inherited_translation_revisions(
        connection,
        root,
        &input.plan.item_id,
        &input.plan.revision_id,
    )? {
        let Some((pair, source)) =
            base_effective_translation(root, &input.plan.item_id, &source.revision_id)?
        else {
            continue;
        };
        if pair.document_analysis_id == input.plan.base_analysis_id {
            continue;
        }
        let base = crate::read_record(root, &pair.document_analysis_id)?;
        if base.body_markdown != input.base.body_markdown
            || source.content_markdown != input.source.content_markdown
        {
            continue;
        }
        let historical = repair_input_for_pair(
            root,
            &input.plan.item_id,
            &input.plan.revision_id,
            &pair,
            source,
        )?;
        candidates.push((historical.plan, historical.source, historical.base));
    }
    for (plan, source, base) in candidates {
        let Some(repair) = read_repair(connection, &plan.repair_id)? else {
            continue;
        };
        let pair = TranslationAnalysisPair {
            title_analysis_id: String::new(),
            document_analysis_id: base.analysis_id.clone(),
        };
        let historical =
            repair_input_for_pair(root, &plan.item_id, &plan.revision_id, &pair, source)?;
        if !same_repair_binding(&historical.plan, &repair.plan) || !repair.model.is_valid() {
            bail!("invalid_translation_table_repair");
        }
        // Pending/unknown ancestors bind the paid identity too. They are never skipped in
        // favour of a new metadata base, even though only succeeded assemblies are displayed.
        return Ok(Some((repair, historical)));
    }
    Ok(None)
}

fn verified_assembly(
    root: &Path,
    input: &RepairInput,
    repair: &Repair,
) -> Result<crate::AnalysisRecord> {
    let expected = repaired_record(input, repair)?;
    if repair.document_analysis_id.as_deref() != Some(expected.analysis_id.as_str())
        || crate::read_record(root, &expected.analysis_id)? != expected
    {
        bail!("invalid_translation_table_repair");
    }
    Ok(expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_numeric_spans_exclude_sentence_punctuation_but_preserve_numeric_syntax() {
        let source = "| Detailed description | Numeric value |\n|---|---:|\n| Please read the detailed report | Values are -$1,234.50, +2.5%, .75, 3/8 and 12:30. |";
        let translated = "| 详细说明文字 | 数值 |\n|---|---:|\n| 请阅读这份详细的报告 | 数值为 -$1,234.50、+2.5%、.75、3/8 和 12:30。 |";
        assert_eq!(translated_table_failure(translated, source), None);
        for (original, changed) in [
            ("1,234.50", "1234.50"), ("1,234.50", "1,23450"),
            ("-$", "$"), ("+2.5%", "2.5%"), ("+2.5%", "+2.5"),
            (".75", "75"), ("3/8", "3:8"), ("12:30", "1230"),
        ] {
            assert_eq!(translated_table_failure(&translated.replace(original, changed), source), Some("table_values"));
        }
        let punctuated = source.replace("12:30.", "12:30: ");
        assert_eq!(translated_table_failure(translated, &punctuated), None);
    }

    #[test]
    fn table_ranges_and_validation_preserve_values_and_ignore_code() {
        let source = "| Description | Source and cost |\n|---|---:|\n| Read the detailed report | [report](https://example.invalid/a(1)) `a\\|b` $2.50 |";
        let translated = "| 说明文字 | 来源及费用 |\n|---|---:|\n| 阅读这份详细的报告 | [报告](https://example.invalid/a(1)) `a\\|b` $2.50 |";
        assert_eq!(translated_table_failure(translated, source), None);
        let normalized = translated.replace("|---|---:|", "|--------|------------:|");
        assert_eq!(translated_table_failure(&normalized, source), None);
        let url = format!("https://example.invalid/{}", "reference-path-".repeat(40));
        let linked_source = format!("| Detailed description | Source |\n|---|---|\n| Please read the detailed [synthetic translation report]({url}) | $2.50 |");
        let linked_translation = format!(
            "| 详细说明文字 | 来源 |\n|---|---|\n| 请阅读这份详细的[合成翻译报告]({url}) | $2.50 |"
        );
        assert_eq!(
            translated_table_failure(&linked_translation, &linked_source),
            None
        );
        for bad in [
            translated.replace("2.50", "2.05"),
            translated.replace("a(1)", "a(2)"),
            translated.replace("`a\\|b`", "`x`"),
            translated.replace("---:", "---"),
            translated.replace("| 说明文字 | 来源及费用 |", "| 说明文字 |"),
            translated.replace("阅读这份详细的报告", ""),
            translated.replace("$2.50", "2.50"),
            "中文说明，不是表格".into(),
        ] {
            assert!(translated_table_failure(&bad, source).is_some());
        }
        let document = format!("Before🙂\r\n\r\n{source}\r\n\r\n````md\n{source}\n````\nAfter");
        let ranges = translation_table_ranges(&document);
        assert_eq!(ranges.len(), 1);
        assert_eq!(&document[ranges[0].clone()], source);
        let signed_source = source.replace("$2.50", "-2.50");
        assert!(
            translated_table_failure(&translated.replace("$2.50", "2.50"), &signed_source)
                .is_some()
        );
        let ragged =
            "| Task description | Estimated cost |\n|---|---:|\n| Read the detailed report |";
        let ragged_zh = "| 任务说明文字 | 预估费用 |\n|---|---:|\n| 阅读这份详细的报告 |";
        assert_eq!(translation_table_ranges(ragged), [0..ragged.len()]);
        assert_eq!(translated_table_failure(ragged_zh, ragged), None);
        for following in [
            "- Keep this list byte-identical",
            "1. Keep this numbered list byte-identical",
            "---\nKeep this paragraph byte-identical",
            "<div>Keep this HTML byte-identical</div>",
        ] {
            let markdown = format!("{source}\n{following}");
            assert_eq!(translation_table_ranges(&markdown), [0..source.len()]);
        }
    }
}
