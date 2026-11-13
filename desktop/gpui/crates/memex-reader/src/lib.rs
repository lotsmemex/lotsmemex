//! Module map: `model` (tabs, blocks, citations; pure), `bilingual` (原文 / 对照 /
//! 译文 layout and block pairing), `translate` (翻译此文 request and polling
//! state), `inline` (inline Markdown and AI highlight spans), `insights`
//! (重点标注 / 金句 / 事实核查 state), `diff` (line and intraline differences;
//! pure), `compare` (对比修改 of two revisions), `vertical` (竖排 columns; pure), `view` (the reader entity and
//! its rendering).
pub mod bilingual;
pub mod compare;
pub mod diff;
pub mod inline;
pub mod insights;
mod image;
mod selection;
pub mod model;
pub mod nav;
pub mod timing;
pub mod translate;
pub mod vertical;
pub mod view;
