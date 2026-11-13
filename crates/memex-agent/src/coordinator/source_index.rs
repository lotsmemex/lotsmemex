//! Derived, in-memory exact URL identities. Store remains the authority: refresh
//! under its snapshot lock, using the same generation/head-stamp contract as
//! LibraryQuery. No titles, bodies, or persistent side index are retained.
use anyhow::{Context, Result};
use memex_core::Sensitivity;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct SourceIndexNotReady;
impl std::fmt::Display for SourceIndexNotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("source_index_not_ready")
    }
}
impl std::error::Error for SourceIndexNotReady {}

#[derive(Default)]
pub(crate) struct SourceIndex {
    root: PathBuf,
    generation: Option<String>,
    items: BTreeMap<String, (memex_store::ItemChangeStamp, String, Option<String>)>,
}

impl SourceIndex {
    /// Caller holds the Store snapshot; never call while holding a queue write
    /// transaction for the initial full verification.
    pub(crate) fn refresh(&mut self, root: &Path) -> Result<()> {
        let result = self.refresh_locked(root);
        if result.is_err() {
            *self = Self::default();
        }
        result
    }

    fn refresh_locked(&mut self, root: &Path) -> Result<()> {
        if self.root != root {
            *self = Self {
                root: root.into(),
                ..Self::default()
            };
        }
        let generation = memex_store::catalog_generation(root)?;
        if generation.is_some() && generation == self.generation {
            return Ok(());
        }
        let mut previous = std::mem::take(&mut self.items);
        self.generation = None;
        for (id, stamp) in memex_store::item_change_stamps(root)? {
            if let Some(entry) = previous.remove(&id).filter(|entry| entry.0 == stamp) {
                self.items.insert(id, entry);
            } else if let Some(metadata) =
                memex_store::read_item_metadata_at_sensitivity(root, &id, Sensitivity::Restricted)?
            {
                self.items
                    .insert(id, (stamp, metadata.source_url, metadata.canonical_url));
            }
        }
        self.generation = generation;
        Ok(())
    }

    /// Matching items are reverified before they can influence an enrichment.
    /// Invalid verification fails closed and discards the derived state.
    pub(crate) fn matching(&mut self, root: &Path, url: &str) -> Result<Vec<String>> {
        self.refresh(root)?;
        self.matching_ready(root, url)
    }

    /// Final commit must return to the caller before refreshing or waiting for
    /// another index builder. It may only verify already prepared matches.
    pub(crate) fn matching_ready(&mut self, root: &Path, url: &str) -> Result<Vec<String>> {
        let generation = memex_store::catalog_generation(root)?;
        if root != self.root || generation != self.generation {
            return Err(SourceIndexNotReady.into());
        }
        if generation.is_none() {
            // A brand-new library has no generation until its first Store write.
            // Do not treat an unversioned populated library as a warm negative.
            match std::fs::symlink_metadata(root.join("items")) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    anyhow::ensure!(
                        std::fs::read_dir(root.join("items"))?.next().is_none()
                            && self.items.is_empty(),
                        "source_index_unversioned"
                    );
                }
                _ => anyhow::bail!("source_index_unversioned"),
            }
        }
        let result = (|| {
            let mut ids = Vec::new();
            for (id, (_, source, canonical)) in &self.items {
                if source != url && canonical.as_deref() != Some(url) {
                    continue;
                }
                let metadata = memex_store::read_item_metadata_at_sensitivity(
                    root,
                    id,
                    Sensitivity::Restricted,
                )?
                .context("source_identity_changed")?;
                anyhow::ensure!(
                    metadata.source_url == url || metadata.canonical_url.as_deref() == Some(url),
                    "source_identity_changed"
                );
                ids.push(id.clone());
            }
            Ok(ids)
        })();
        if result.is_err() {
            *self = Self::default();
        }
        result
    }
}
