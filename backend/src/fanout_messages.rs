//! Inbox, toast and push copy as localizable messages.
//!
//! Core renders a notify hint's `i18n` text per recipient (the inbox row and
//! toast in the node's `ui-language`, each push in the locale its device
//! registered with); the English rendering still goes out as the plain
//! `title`/`body`, which an older Core delivers as is.
//!
//! Each `*_MESSAGE` is `format!` with one named argument per placeholder
//! (`name = arg("name")`) so the i18n extractor catalogs the ICU source;
//! `tools/generate-core-i18n.mjs` copies the translations into Core's catalog.
//! Every message is a whole sentence. Core's renderer has no plural selector,
//! so counts are written as `label: {count}`.
#![allow(clippy::useless_format)]

use ryu_app_events::{NotifyHint, NotifyMessage, NotifyTemplate as Template};

pub(crate) const REPLAY_DONE_MESSAGE: Template =
    |arg| format!("Recipe “{name}” replay done", name = arg("name"));
pub(crate) const REPLAY_FAILED_MESSAGE: Template =
    |arg| format!("Recipe “{name}” replay failed", name = arg("name"));

/// A recipe replay finished.
pub(crate) fn replay_done(name: &str) -> NotifyHint {
    NotifyHint::localized(
        NotifyMessage::new(REPLAY_DONE_MESSAGE).text("name", name),
        Vec::new(),
    )
    .with_level("success")
}

/// A recipe replay failed; the body relays the error.
pub(crate) fn replay_failed(name: &str, error: &str) -> NotifyHint {
    NotifyHint::localized(
        NotifyMessage::new(REPLAY_FAILED_MESSAGE).text("name", name),
        vec![NotifyMessage::relayed(error)],
    )
    .with_level("error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_replay_relays_its_error_for_core_to_translate() {
        let hint = replay_failed("Weekly report", "step 2 timed out");
        assert_eq!(hint.body.as_deref(), Some("step 2 timed out"));
        let body = &serde_json::to_value(&hint).unwrap()["i18n"]["body"][0];
        assert_eq!(body["relay"], true);
    }
}
