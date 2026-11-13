//! A visible reader image. Byte transport and decoding run off the UI thread.
use gpui_kit::*;
use gpui_kit::component::ActiveTheme;
use std::{io::Cursor, sync::Arc};

pub struct ReaderImage {
    image: Option<Arc<Image>>,
    status: &'static str,
}

impl ReaderImage {
    pub fn new(client: memex_client::Client, item: String, revision: String, target: String, cx: &mut Context<Self>) -> Self {
        let task = cx.background_executor().spawn(async move {
            let bytes = client.image_read(&item, &revision, &target).map_err(|_| "图片不可用：附件缺失、无权限或版本已变化")?;
            let reader = image::ImageReader::new(Cursor::new(&bytes)).with_guessed_format().map_err(|_| "图片格式无法解码")?;
            let (w, h) = reader.into_dimensions().map_err(|_| "图片格式无法解码")?;
            if w == 0 || h == 0 || u64::from(w) * u64::from(h) > 16_000_000 { return Err("图片尺寸过大，暂不显示") }
            // A single bounded raster, including the first frame of GIF/WebP;
            // do not let an unbounded animation enter the GPUI image cache.
            let decoded = image::load_from_memory(&bytes).map_err(|_| "图片格式无法解码")?;
            let mut png = Cursor::new(Vec::new());
            decoded.write_to(&mut png, image::ImageFormat::Png).map_err(|_| "图片格式无法解码")?;
            Ok(Arc::new(Image::from_bytes(ImageFormat::Png, png.into_inner())))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result { Ok(image) => this.image = Some(image), Err(status) => this.status = status }
                cx.notify();
            }).ok();
        }).detach();
        Self { image: None, status: "正在载入本地图片…" }
    }
}

impl Render for ReaderImage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = div().w_full().h(px(320.)).my_2().flex().items_center().justify_center();
        match &self.image {
            Some(image) => frame.child(img(image.clone()).w_full().h_full().object_fit(ObjectFit::Contain)),
            None => frame.text_sm().text_color(cx.theme().muted_foreground).child(memex_client::tx(self.status)),
        }
    }
}
