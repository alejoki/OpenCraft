//! Pure pack-content data model.
//!
//! These types are the parser's output and the registry builder's input. They
//! deliberately contain no filesystem, pack, settings, audio or GPU
//! dependency: content identity must stay decoupled from presentation.
//!
//! Every entry is the *accepted* form of an authored array entry. The pack
//! parser rejects an entry individually (recording a [`ContentDiagnostic`])
//! while keeping its valid siblings, so a spec never contains a half-parsed
//! definition.
//!
//! The model and the neutral [`ToolKind`] live in `opencraft-content`; this
//! module is the historical game path and only re-exports them.

pub use opencraft_content::spec::*;
