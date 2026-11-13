//! Module map: `model` (rows, filters, pages; pure), `edit` (group writes:
//! request identity and outcomes; pure), `sources` (来源 grouped by site; pure), `import` (file drop target and import tray state; pure), `capture` (URL capture progress for the 采集 card; pure), `import_view` (the 采集 card — URL captures and file imports — and sidebar drop targets), `tree` (folder tree, inline editing,
//! breadcrumb, 加入分组 picker), `trash` (回收站 list and restore), `view`
//! (library entity, sidebar, classic table, reading list).
pub mod edit;
pub mod import;
pub mod import_view;
pub mod capture;
pub mod model;
pub mod sources;
pub mod trash;
pub mod tree;
pub mod view;
