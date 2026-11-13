//! Supplemental comments are evidence, never a second article or model input.
use super::{validate_inert_dom_snapshot, PreparedCapture};
use anyhow::{bail, Result};
use memex_core::{BlobDescriptor, Sensitivity, VerifiedBlob};
pub use lots_scout_extract::CommentsState;
use sha2::{Digest, Sha256};

pub const MAX_COMMENTS_BYTES: usize = 2 * 1024 * 1024;

pub struct Comments {
    pub state: CommentsState,
    pub html: Option<Vec<u8>>,
}

impl Comments {
    pub fn extract(html: &str) -> Result<Self> {
        let extracted = lots_scout_extract::extract_comments(html, MAX_COMMENTS_BYTES)?;
        let comments = Self {
            state: extracted.state,
            html: extracted.html.map(String::into_bytes),
        };
        comments.validate()?;
        Ok(comments)
    }

    pub fn validate(&self) -> Result<()> {
        match (&self.state, &self.html) {
            (CommentsState::Captured, Some(bytes))
                if !bytes.is_empty() && bytes.len() <= MAX_COMMENTS_BYTES =>
            {
                validate_inert_dom_snapshot(std::str::from_utf8(bytes)?)
            }
            (CommentsState::Captured, _) | (_, Some(_)) => bail!("invalid comments observation"),
            (_, None) => Ok(()),
        }
    }
}

impl PreparedCapture {
    pub fn extract_comments(&self) -> Result<Comments> {
        if lots_scout_extract::LinkresearcherArticleExtractor::supports(&self.capture) {
            // Parse the article field, never interpret raw JSON (including
            // recommendations or escaped markup) as a document snapshot.
            Comments::extract(
                &lots_scout_extract::LinkresearcherArticleExtractor::article_html(&self.capture)?,
            )
        } else {
            Comments::extract(std::str::from_utf8(self.artifact_bytes())?)
        }
    }

    pub fn with_comments(mut self, comments: Comments, backend: &str) -> Result<Self> {
        comments.validate()?;
        if !matches!(backend, "memex-intake-v1" | "agent-cas") {
            bail!("invalid comments backend");
        }
        if self.capture.blobs.iter().any(|blob| {
            matches!(
                blob.descriptor.locator.as_str(),
                "comments_observation_v1" | "comments_html_v1"
            )
        }) {
            bail!("duplicate comments observation");
        }
        // New supplemental capture retains the source's Internal floor. Legacy captures
        // without this opt-in are unchanged; callers cannot lower the new source semantics.
        self.capture.capture.sensitivity =
            self.capture.capture.sensitivity.max(Sensitivity::Internal);
        self.draft.sensitivity = self.draft.sensitivity.max(Sensitivity::Internal);
        if self.capture.capture.artifacts.is_empty() {
            self.capture.capture.artifacts = self
                .capture
                .blobs
                .iter()
                .map(|blob| blob.descriptor.clone())
                .collect();
        }
        let observation = serde_json::to_vec(&comments.state)?;
        self.add_comment_blob(
            "comments_observation_v1",
            "application/json",
            observation,
            backend,
        );
        if let Some(bytes) = comments.html {
            self.add_comment_blob("comments_html_v1", "text/html", bytes, backend);
        }
        Ok(self)
    }

    fn add_comment_blob(&mut self, locator: &str, media_type: &str, bytes: Vec<u8>, backend: &str) {
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let descriptor = BlobDescriptor {
            schema_version: 1,
            locator: locator.into(),
            media_type: media_type.into(),
            byte_size: bytes.len() as u64,
            sha256: digest.clone(),
            source_url: None,
        };
        self.capture.capture.artifacts.push(descriptor.clone());
        self.capture.blobs.push(VerifiedBlob {
            descriptor,
            blob_id: format!("sha256:{digest}"),
            source_backend_ref: Some(format!("{backend}:sha256:{digest}")),
            bytes,
        });
    }
}
