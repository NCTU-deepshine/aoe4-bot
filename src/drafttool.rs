//! Reads from the external draft tool (docs/tournament.md §3).
//!
//! **Unauthenticated only.** A preset's config is public (§3.1:
//! `GET /api/presets/:id` returns the full config for any public preset), which
//! is all setup needs to learn a round's `bestOf`. Chunk 14 owns the Auth.js
//! handshake and `POST /api/matches`; keeping that out of here is what lets
//! tournament setup work without any of Phase E.

use reqwest::{Client, Url};
use serde::Deserialize;
use std::sync::OnceLock;
use tracing::error;

/// §3's documented instance. Overridable so a self-hosted fork (§12) can be
/// pointed at without a code change — the same reason `tournaments` carries a
/// nullable `draft_base_url` column nothing writes yet.
const DEFAULT_BASE_URL: &str = "https://aoe4banpick-production.up.railway.app";

fn base_url() -> String {
    std::env::var("DRAFT_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .build()
            .expect("failed to build draft tool http client")
    })
}

/// A preset as the tool returns it. Only the fields setup actually reads are
/// modelled; the config also carries full civ and map catalogues, which are the
/// tool's business (§2) and none of ours.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Preset {
    pub name: String,
    pub is_public: bool,
    pub config: PresetConfig,
}

#[derive(Deserialize, Debug)]
pub(crate) struct PresetConfig {
    pub options: PresetOptions,
}

/// `resultMode` lives here, beside `bestOf` — **not** at the top level, which is
/// what §3.3's phrasing suggests. Verified against the live endpoint.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PresetOptions {
    pub best_of: i64,
    pub result_mode: String,
}

pub(crate) async fn fetch_preset(preset_id: &str) -> Option<Preset> {
    let url = format!("{}/api/presets/{preset_id}", base_url());
    let url = Url::parse(&url)
        .inspect_err(|err| error!("draft tool preset url {url} is invalid: {err}"))
        .ok()?;

    let response = client()
        .get(url)
        .send()
        .await
        .inspect_err(|err| error!("draft tool preset request failed: {err}"))
        .ok()?;

    // A missing or private preset is a 404, which is a real answer rather than a
    // transport failure — the caller reports it as "no such preset".
    if !response.status().is_success() {
        return None;
    }

    response
        .json::<Preset>()
        .await
        .inspect_err(|err| error!("draft tool preset decode failed: {err}"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::Preset;

    /// A real response, trimmed (`src/tournament/testdata/`). §10: deserialization
    /// is tested against a saved payload, never live.
    fn preset() -> Preset {
        serde_json::from_str(include_str!("tournament/testdata/draft_preset.json"))
            .expect("the saved preset payload should parse")
    }

    #[test]
    fn reads_best_of_and_result_mode_from_options() {
        // Both live under config.options, which is the correction the live check
        // turned up against §3.3's wording.
        let preset = preset();
        assert_eq!(preset.config.options.best_of, 3);
        assert_eq!(preset.config.options.result_mode, "vote");
        assert!(preset.is_public);
        assert!(!preset.name.is_empty());
    }

    #[tokio::test]
    #[ignore = "hits the live draft tool API"]
    async fn the_live_endpoint_still_answers_with_a_preset() {
        // The one thing a saved payload cannot prove: that the shape still holds.
        let preset = super::fetch_preset("6a4325f095d3637a0d064c21")
            .await
            .expect("expected a public preset");
        assert!(preset.config.options.best_of % 2 == 1, "bestOf should be odd");
    }

    #[tokio::test]
    #[ignore = "hits the live draft tool API"]
    async fn an_unknown_preset_is_none_rather_than_an_error() {
        assert!(super::fetch_preset("000000000000000000000000").await.is_none());
    }
}
