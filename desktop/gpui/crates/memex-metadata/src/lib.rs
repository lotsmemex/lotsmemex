//! Module map: `session` (title/tag drafts, unchanged writer), `biblio`
//! (bibliographic edit draft, pure), `lookup` (补全预览 state and rules, pure),
//! `biblio_view` (bibliographic fields entity), `lookup_view` (preview sheet),
//! `view` (the metadata card that composes them).
pub mod biblio;
pub mod biblio_view;
pub mod lookup;
mod lookup_view;
pub mod session;
pub mod view;
