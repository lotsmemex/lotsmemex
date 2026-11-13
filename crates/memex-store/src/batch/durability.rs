//! Grouped device barriers for the existing v1 journal protocol.
//!
//! On macOS fsync sends a file to the device, whereas F_FULLFSYNC drains the
//! entire device queue. Every file/directory is still fsynced; a full barrier
//! separates staging, publication, installation, commit, and cleanup. No stage
//! becomes a recoverable prepared journal before its complete bytes are durable.
use super::*;

fn sync_file(file: &fs::File) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        loop {
            if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        file.sync_all()?;
        Ok(())
    }
}

pub(super) fn sync_directory(path: &Path) -> Result<()> {
    sync_file(&fs::File::open(path)?)
}

pub(super) fn same_device(root: &Path, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(
            fs::metadata(root)?.dev() == fs::metadata(path)?.dev(),
            "batch journal and outputs must be on one device"
        );
    }
    Ok(())
}

// std::File::sync_all uses F_FULLFSYNC on macOS. The same-device guard is
// required: one drive barrier cannot make another drive's writes durable.
pub(super) fn barrier(root: &Path) -> Result<()> {
    sync_dir(root)
}

fn directory(root: &Path, relative: &Path) -> Result<PathBuf> {
    let path = ensure_contained_dir_with_sync(root, relative, sync_directory)?;
    same_device(root, &path)?;
    Ok(path)
}

fn write_new(root: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    same_device(root, path.parent().context("batch file has no parent")?)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    sync_file(&file)
}

pub(super) fn stage(root: &Path, plan: IngestPlan) -> Result<PreparedIngest> {
    let IngestPlan::New(journal, contents) = plan else {
        let IngestPlan::Replay(receipt) = plan else {
            unreachable!()
        };
        return Ok(PreparedIngest::Replay(receipt));
    };
    let parent = root.join("catalog/ingest-journal");
    let temporary = parent.join(format!(".prepare-{}", new_id("journal")));
    fs::create_dir(&temporary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700))?;
    }
    let entries = temporary.join("entries");
    fs::create_dir(&entries)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&entries, fs::Permissions::from_mode(0o700))?;
    }
    for (entry, bytes) in journal.entries.iter().zip(contents) {
        write_new(root, &temporary.join(&entry.staged), &bytes)?;
    }
    write_new(
        root,
        &temporary.join("journal.json"),
        &serde_json::to_vec_pretty(&journal)?,
    )?;
    sync_directory(&entries)?;
    sync_directory(&temporary)?;
    sync_directory(&parent)?;
    Ok(PreparedIngest::Journal(temporary, journal))
}

pub(super) fn publish(root: &Path, prepared: &mut [PreparedIngest]) -> Result<()> {
    let parent = root.join("catalog/ingest-journal");
    for item in prepared {
        if let PreparedIngest::Journal(path, journal) = item {
            let target = parent.join(journal_key_name(&journal.ingest_key)?);
            anyhow::ensure!(!target.try_exists()?, "ingest journal already exists");
            fs::rename(&*path, &target)?;
            *path = target;
        }
    }
    sync_directory(&parent)?;
    barrier(root)
}

pub(super) fn install(root: &Path, prepared: PreparedIngest) -> Result<PreparedIngest> {
    if let PreparedIngest::Journal(path, journal) = &prepared {
        for relative in &journal.directories {
            directory(root, Path::new(relative))?;
        }
        for entry in &journal.entries {
            let staged = path.join(&entry.staged);
            let mut bytes = Vec::with_capacity(entry.byte_size as usize);
            open_regular_nofollow(&staged)?.read_to_end(&mut bytes)?;
            anyhow::ensure!(
                bytes.len() as u64 == entry.byte_size
                    && format!("sha256:{:x}", Sha256::digest(&bytes)) == entry.sha256,
                "ingest journal staged content changed"
            );
            let output =
                contained_output_path_with_sync(root, &root.join(&entry.path), sync_directory)?;
            same_device(root, output.parent().context("batch output has no parent")?)?;
            // The durable stage is already the final immutable file. Publish
            // its inode without a second copy/write; never overwrite an output.
            match fs::hard_link(&staged, &output) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    verify_binary(&output, &bytes)?;
                }
                Err(error) => return Err(error.into()),
            }
            sync_directory(output.parent().context("batch output has no parent")?)?;
        }
    }
    Ok(prepared)
}

pub(super) fn commit(root: &Path, prepared: &mut [PreparedIngest]) -> Result<()> {
    let mut replacements = Vec::new();
    for item in prepared {
        if let PreparedIngest::Journal(path, journal) = item {
            journal.state = JournalState::Committed;
            let temporary = path.join(format!(".journal-{}.tmp", new_id("write")));
            write_new(root, &temporary, &serde_json::to_vec_pretty(journal)?)?;
            sync_directory(path)?;
            replacements.push((temporary, path.join("journal.json")));
        }
    }
    // Never replace the durable prepared record with data still in drive cache.
    // Interrupted pre-publication temporaries are recognized by v1 recovery.
    barrier(root)?;
    for (temporary, target) in replacements {
        fs::rename(temporary, &target)?;
        sync_directory(target.parent().context("batch journal has no parent")?)?;
    }
    barrier(root)
}

pub(super) fn archive(root: &Path, prepared: PreparedIngest) -> Result<IngestReceipt> {
    match prepared {
        PreparedIngest::Replay(receipt) => Ok(receipt),
        PreparedIngest::Journal(path, journal) => {
            cleanup_journal_staging_with_sync(&path, sync_directory)?;
            archive_committed_journal_with_sync(root, &path, sync_directory)?;
            Ok(journal.receipt)
        }
    }
}
