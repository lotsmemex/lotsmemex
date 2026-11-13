use super::*;

const RSS: &[u8] =
    b"<rss><channel><item><link>https://example.invalid/episode</link></item></channel></rss>";
const PRIVATE: Sensitivity = Sensitivity::Private;

fn raster(width: u32, height: u32, color: u8, format: ImageFormat) -> Vec<u8> {
    let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        width,
        height,
        image::Rgb([color, 21, 39]),
    ));
    let mut buffer = Cursor::new(Vec::new());
    image.write_to(&mut buffer, format).unwrap();
    buffer.into_inner()
}

fn fixture(source_url: &str) -> (tempfile::TempDir, CoverEdit) {
    let dir = tempfile::tempdir().unwrap();
    let mut capture = crate::tests::capture();
    let mut draft = crate::tests::draft();
    capture.capture.sensitivity = PRIVATE;
    draft.sensitivity = PRIVATE;
    draft.source_url = source_url.into();
    capture.capture.source_url = draft.source_url.clone();
    let receipt = ingest(dir.path(), &capture, draft).unwrap();
    (
        dir,
        CoverEdit {
            item_id: receipt.item_ids[0].clone(),
            expected_revision_id: receipt.revision_ids[0].clone(),
            expected_cover_version: None,
            request_id: "cover-first".into(),
            source: CoverSource::Podcast {
                episode_url: source_url.into(),
                youtube_video_id: "abcdefghijk".into(),
                image_url: "https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg".into(),
                rss_sha256: hash(RSS),
            },
        },
    )
}

#[test]
fn independent_cover_cas_replay_and_authorized_thumbnail() {
    let (dir, edit) = fixture("https://example.invalid/episode");
    let root = dir.path();
    let before = serde_json::to_vec(&read_item(root, &edit.item_id).unwrap()).unwrap();
    let bytes = raster(640, 360, 87, ImageFormat::Png);
    let first = put_cover(root, edit.clone(), &bytes, RSS, PRIVATE).unwrap();
    assert_eq!((first.thumbnail.width, first.thumbnail.height), (320, 180));
    assert_eq!(
        first,
        put_cover(root, edit.clone(), &bytes, RSS, PRIVATE).unwrap()
    );
    assert_eq!(
        before,
        serde_json::to_vec(&read_item(root, &edit.item_id).unwrap()).unwrap()
    );
    let preview = cover_chunk(root, &edit.item_id, &first.version, true, 0, PRIVATE).unwrap();
    assert_eq!(
        image::load_from_memory(&preview.bytes).unwrap().width(),
        320
    );
    let items = vec![edit.item_id.clone()];
    assert!(cover_batch(root, &items, Sensitivity::Public).unwrap()[0].is_none());
    assert!(cover_chunk(
        root,
        &edit.item_id,
        &first.version,
        true,
        0,
        Sensitivity::Public
    )
    .is_err());
    assert!(put_cover(root, edit.clone(), &bytes, RSS, Sensitivity::Public).is_err());
    let alternate = raster(640, 360, 173, ImageFormat::Png);
    assert_eq!(
        put_cover(root, edit.clone(), &alternate, RSS, PRIVATE)
            .unwrap_err()
            .to_string(),
        "request_id_conflict"
    );
    let mut second = edit.clone();
    second.request_id = "cover-second".into();
    assert_eq!(
        put_cover(root, second.clone(), &alternate, RSS, PRIVATE)
            .unwrap_err()
            .to_string(),
        "stale_cover_version"
    );
    second.expected_cover_version = Some(first.version.clone());
    let next = put_cover(root, second, &alternate, RSS, PRIVATE).unwrap();
    assert_ne!(next.version, first.version);
    assert_eq!(
        put_cover(root, edit.clone(), &bytes, RSS, PRIVATE).unwrap(),
        first
    );
    assert_eq!(cover_batch(root, &items, PRIVATE).unwrap()[0], Some(next));
    assert!(cover_chunk(root, &edit.item_id, &first.version, false, 0, PRIVATE).is_err());
    trash_item(
        root,
        &edit.item_id,
        &edit.expected_revision_id,
        "duplicate",
        None,
        Some("fixture"),
    )
    .unwrap();
    assert!(put_cover(root, edit.clone(), &bytes, RSS, PRIVATE).is_err());
    assert!(cover_batch(root, &items, PRIVATE).unwrap()[0].is_none());
}

#[test]
fn interrupted_publication_and_moved_backup_recover_without_source_paths() {
    for step in [1, 2] {
        let (dir, edit) = fixture("https://example.invalid/episode");
        let bytes = raster(640, 360, 32, ImageFormat::Jpeg);
        INTERRUPT.with(|s| s.set(Some(step)));
        let result = put_cover(dir.path(), edit.clone(), &bytes, RSS, PRIVATE);
        INTERRUPT.with(|s| s.set(None));
        assert_eq!(
            result.unwrap_err().to_string(),
            "simulated_cover_interruption"
        );
        let seen = cover_batch(dir.path(), &[edit.item_id.clone()], PRIVATE).unwrap();
        assert_eq!(seen[0].is_some(), step == 2);
        // Stop all writes, copy the WHOLE library, then remove the original root.
        fn copy_tree(from: &Path, to: &Path) {
            fs::create_dir_all(to).unwrap();
            for entry in fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let target = to.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    copy_tree(&entry.path(), &target)
                } else {
                    fs::copy(entry.path(), target).unwrap();
                }
            }
        }
        let backup = tempfile::tempdir().unwrap();
        let restored = backup.path().join("restored");
        copy_tree(dir.path(), &restored);
        dir.close().unwrap();
        let result = put_cover(&restored, edit.clone(), &bytes, RSS, PRIVATE).unwrap();
        if let Some(prior) = &seen[0] {
            assert_eq!(prior, &result)
        }
        assert_eq!(
            cover_chunk(&restored, &edit.item_id, &result.version, false, 0, PRIVATE)
                .unwrap()
                .bytes,
            bytes
        );
        let thumbnail =
            cover_chunk(&restored, &edit.item_id, &result.version, true, 0, PRIVATE).unwrap();
        assert_eq!(hash(&thumbnail.bytes), result.thumbnail.sha256);
        assert_eq!(
            image::load_from_memory(&thumbnail.bytes).unwrap().height(),
            180
        );
        assert_eq!(
            read_bounded(
                &restored,
                &resource_path(&restored, result.source.evidence_sha256()).unwrap(),
                4096
            )
            .unwrap(),
            RSS
        );
        // Same-size replacement is detected; mtime and filename are not proof.
        let path = resource_path(&restored, &result.raster.sha256).unwrap();
        let mut corrupt = bytes.clone();
        corrupt[30] ^= 1;
        fs::write(&path, corrupt).unwrap();
        assert!(cover_chunk(&restored, &edit.item_id, &result.version, false, 0, PRIVATE).is_err());
        fs::remove_file(&path).unwrap();
        let outside = backup.path().join("outside.jpg");
        fs::write(&outside, &bytes).unwrap();
        std::os::unix::fs::symlink(outside, path).unwrap();
        assert!(cover_chunk(&restored, &edit.item_id, &result.version, false, 0, PRIVATE).is_err());
    }
}

#[test]
fn raster_and_source_bounds_reject_spoofs_and_placeholders() {
    let (dir, edit) = fixture("https://example.invalid/episode");
    for format in [ImageFormat::Jpeg, ImageFormat::Png, ImageFormat::WebP] {
        let mut request = edit.clone();
        request.request_id = format!("format-{format:?}");
        let result = put_cover(
            dir.path(),
            request,
            &raster(640, 360, 70, format),
            RSS,
            PRIVATE,
        );
        if format == ImageFormat::Jpeg {
            assert!(result.is_ok())
        } else {
            assert_eq!(result.unwrap_err().to_string(), "stale_cover_version")
        }
    }
    for bytes in [
        b"\x89PNG\r\n\x1a\n".to_vec(),
        vec![0; MAX_COVER_BYTES + 1],
        b"<svg/>".to_vec(),
    ] {
        assert!(put_cover(dir.path(), edit.clone(), &bytes, RSS, PRIVATE).is_err());
    }
    assert_eq!(
        put_cover(
            dir.path(),
            edit.clone(),
            &raster(120, 90, 0, ImageFormat::Png),
            RSS,
            PRIVATE
        )
        .unwrap_err()
        .to_string(),
        "cover_placeholder"
    );
    // Compact, valid image whose decompressed pixel count is over budget.
    assert_eq!(
        put_cover(
            dir.path(),
            edit.clone(),
            &raster(5000, 4001, 0, ImageFormat::Png),
            RSS,
            PRIVATE
        )
        .unwrap_err()
        .to_string(),
        "invalid_cover_image"
    );
    let CoverSource::Podcast { image_url, .. } = &edit.source else { panic!("podcast fixture"); };
    assert!(youtube_cover_url("abcdefghijk", image_url));
    for url in [
        "http://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg",
        "https://i.ytimg.com.evil/vi/abcdefghijk/hqdefault.jpg",
        "https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg?redirect=local",
        "https://127.0.0.1/hqdefault.jpg",
        "https://i.ytimg.com/vi/anotherabcd/hqdefault.jpg",
        "https://user@i.ytimg.com/vi/abcdefghijk/hqdefault.jpg",
    ] {
        assert!(!youtube_cover_url("abcdefghijk", url));
    }
}

#[test]
fn weixin_cover_has_exact_source_shape_and_keeps_cas_replay_and_permissions() {
    let (dir, mut edit) = fixture("https://mp.weixin.qq.com/s/synthetic");
    let html = b"<div id='js_content'>Synthetic interview</div>";
    edit.source = CoverSource::Weixin { article_url:"https://mp.weixin.qq.com/s/synthetic".into(),
        image_url:"https://mmbiz.qpic.cn/mmbiz_jpg/synthetic/0?wx_fmt=jpeg".into(), html_sha256:hash(html) };
    let source = serde_json::to_value(&edit.source).unwrap();
    assert_eq!(source.as_object().unwrap().len(), 3);
    assert_eq!(serde_json::from_value::<CoverSource>(source.clone()).unwrap(), edit.source);
    let mut mixed = source; mixed["rss_sha256"] = serde_json::json!(hash(RSS));
    assert!(serde_json::from_value::<CoverSource>(mixed).is_err());
    let before = serde_json::to_vec(&read_item(dir.path(), &edit.item_id).unwrap()).unwrap();
    let image = raster(240, 240, 81, ImageFormat::Png);
    let first = put_cover(dir.path(), edit.clone(), &image, html, PRIVATE).unwrap();
    assert_eq!(put_cover(dir.path(), edit.clone(), &image, html, PRIVATE).unwrap(), first);
    assert_eq!(before, serde_json::to_vec(&read_item(dir.path(), &edit.item_id).unwrap()).unwrap());
    assert!(cover_batch(dir.path(), &[edit.item_id.clone()], Sensitivity::Public).unwrap()[0].is_none());
    assert!(put_cover(dir.path(), edit.clone(), &image, html, Sensitivity::Public).is_err());
    assert!(put_cover(dir.path(), edit.clone(), &image, b"changed", PRIVATE).is_err());
    let mut other = edit.clone(); other.request_id = "other-cover".into();
    assert_eq!(put_cover(dir.path(), other, &image, html, PRIVATE).unwrap_err().to_string(), "stale_cover_version");
    let mut stale = edit.clone(); stale.request_id = "stale-revision".into();
    stale.expected_revision_id = new_id("revision");
    assert_eq!(put_cover(dir.path(), stale, &image, html, PRIVATE).unwrap_err().to_string(), "stale_item_revision");
    for url in ["https://mmbiz.qpic.cn.evil/mmbiz_jpg/a/0", "http://mmbiz.qpic.cn/mmbiz_jpg/a/0",
        "https://user@mmbiz.qpic.cn/mmbiz_jpg/a/0", "https://mmbiz.qpic.cn/mmbiz_jpg/a/0#x",
        "https://mmbiz.qpic.cn/other/a/0", "https://127.0.0.1/mmbiz_jpg/a/0"] {
        assert!(!weixin_image_url(url));
    }
}

#[test]
fn podcast_cover_source_preserves_legacy_receipt_serialization() {
    // Literal former struct wire bytes, including field order. The cover request
    // digest serializes this value inside CoverEdit; enum tags would break replay.
    const LEGACY: &str = r#"{"episode_url":"https://example.invalid/episode","youtube_video_id":"abcdefghijk","image_url":"https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg","rss_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#;
    let source: CoverSource = serde_json::from_str(LEGACY).unwrap();
    assert!(matches!(source, CoverSource::Podcast { .. }));
    assert_eq!(serde_json::to_string(&source).unwrap(), LEGACY);
}
