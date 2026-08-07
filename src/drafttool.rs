//! The external draft tool (docs/tournament.md §3), read and write.
//!
//! Reading a preset needs no session — a public preset's config is public (§3.1)
//! — which is what lets tournament setup work without any of Phase E. Creating a
//! match does, and the session is an Auth.js cookie, so the client is held rather
//! than rebuilt.
//!
//! The tool also has a no-account guest path, and it is deliberately unused:
//! `GUEST_MATCHES_PER_HOUR = 5` per IP and `GUEST_OPEN_LOBBIES = 3`
//! (`lib/features.ts`) would stall a 16-player bracket's 15 sets inside round one.

use reqwest::{Client, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use tracing::error;

/// §3's documented instance. Overridable so a self-hosted fork (§12) can be
/// pointed at without a code change — the same reason `tournaments` carries a
/// nullable `draft_base_url` column nothing writes yet.
const DEFAULT_BASE_URL: &str = "https://aoe4banpick-production.up.railway.app";

pub(crate) fn base_url() -> String {
    std::env::var("DRAFT_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            // The Auth.js session is a cookie, so it has to survive from the
            // handshake to the request it authorizes.
            .cookie_store(true)
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

/// What went wrong talking to the draft tool, split by what a caller has to do
/// about it rather than by where it happened.
#[derive(Debug)]
pub(crate) enum DraftError {
    /// No `DRAFT_USERNAME`/`DRAFT_PASSWORD`. Draft creation is unavailable; the
    /// rest of the bot is not, which is why this is not a panic at startup.
    NotConfigured,
    /// The tool refused us even after a fresh sign-in — wrong credentials, or an
    /// account that can no longer log in.
    Unauthorized,
    /// The preset is private and belongs to somebody else.
    Forbidden,
    /// The tool's own `validatePreset` refused the preset. We cannot run that
    /// check ahead of time (§3.3), so keeping its issues is the only way anyone
    /// learns which rule was broken.
    PresetRejected {
        issues: Vec<PresetIssue>,
    },
    Transport(String),
}

impl std::fmt::Display for DraftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => write!(f, "the draft tool credentials are not configured"),
            Self::Unauthorized => write!(f, "the draft tool refused our credentials"),
            Self::Forbidden => write!(f, "the draft tool refused access to that preset"),
            Self::PresetRejected { issues } => write!(f, "the draft tool rejected the preset: {issues:?}"),
            Self::Transport(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for DraftError {}

/// One reason a preset is unplayable, as the tool reports it.
///
/// A code plus its numbers rather than a finished sentence: the tool's preset
/// authors read Chinese and Japanese, so it leaves the wording to its own i18n
/// (`lib/draft/validate.ts`). We have no access to those strings, so the code is
/// what we can show.
#[derive(Deserialize, Debug, PartialEq, Eq)]
pub(crate) struct PresetIssue {
    pub code: String,
    #[serde(default)]
    pub params: Option<HashMap<String, serde_json::Value>>,
}

/// The room the tool opened. `id` is our `draft_external_id`, and both the room
/// and spectator links are derived from it.
///
/// The response also carries a `shareCode`; nothing in the tool or here consumes
/// it (§3.2), and serde drops unknown fields, so it is not modelled.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreatedMatch {
    pub id: String,
}

struct Credentials {
    username: String,
    password: String,
}

/// Split from `from_env` so the "not configured" decision is testable without
/// mutating process environment, which no test can do safely in parallel.
fn credentials_from(username: Option<String>, password: Option<String>) -> Result<Credentials, DraftError> {
    match (username, password) {
        (Some(username), Some(password)) if !username.trim().is_empty() && !password.is_empty() => {
            Ok(Credentials { username, password })
        },
        _ => Err(DraftError::NotConfigured),
    }
}

impl Credentials {
    fn from_env() -> Result<Self, DraftError> {
        credentials_from(
            std::env::var("DRAFT_USERNAME").ok(),
            std::env::var("DRAFT_PASSWORD").ok(),
        )
    }
}

/// Opens a room on the tool for `preset_id`, signing in if the session has gone.
pub(crate) async fn create_match(preset_id: &str) -> Result<CreatedMatch, DraftError> {
    let credentials = Credentials::from_env()?;
    create_match_at(client(), &base_url(), &credentials, preset_id).await
}

/// The base url and client are parameters so the tests can point at a local stub
/// rather than setting environment variables a parallel test would race on.
async fn create_match_at(
    client: &Client,
    base: &str,
    credentials: &Credentials,
    preset_id: &str,
) -> Result<CreatedMatch, DraftError> {
    let response = post_match(client, base, preset_id).await?;

    // A session expires, so the first 401 is ordinary and worth one retry.
    // Exactly one: a 401 for any other reason would make a loop out of this, and
    // hammering the tool we depend on is worse than failing the set.
    let response = if response.status() == StatusCode::UNAUTHORIZED {
        sign_in_at(client, base, credentials).await?;
        post_match(client, base, preset_id).await?
    } else {
        response
    };

    match response.status() {
        StatusCode::OK | StatusCode::CREATED => response.json::<CreatedMatch>().await.map_err(transport),
        StatusCode::UNAUTHORIZED => Err(DraftError::Unauthorized),
        StatusCode::FORBIDDEN => Err(DraftError::Forbidden),
        StatusCode::BAD_REQUEST => Err(preset_rejected(response).await),
        status => Err(DraftError::Transport(format!(
            "draft tool answered {status} creating a match"
        ))),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateMatchBody<'a> {
    preset_id: &'a str,
}

async fn post_match(client: &Client, base: &str, preset_id: &str) -> Result<Response, DraftError> {
    client
        .post(format!("{base}/api/matches"))
        .json(&CreateMatchBody { preset_id })
        .send()
        .await
        .map_err(transport)
}

/// Auth.js v5's credentials flow: a CSRF token, then the callback, then a check.
///
/// **The callback's status proves nothing.** A wrong password is answered with a
/// 302 back to the sign-in page carrying `?error=`, which a redirect-following
/// client reports as a perfectly good 200 — so the session is probed instead.
/// Cookie names are never mentioned anywhere here: the `__Secure-` prefix
/// differs between the https instance and a local one, and the store keeps
/// whatever it is handed either way.
async fn sign_in_at(client: &Client, base: &str, credentials: &Credentials) -> Result<(), DraftError> {
    let csrf: Csrf = get_json(client, &format!("{base}/api/auth/csrf")).await?;

    client
        .post(format!("{base}/api/auth/callback/credentials"))
        .form(&[
            ("csrfToken", csrf.csrf_token.as_str()),
            // `email` is the field name, but the tool matches it against email
            // *or* username (`auth.ts`), so either works here.
            ("email", credentials.username.as_str()),
            ("password", credentials.password.as_str()),
        ])
        .send()
        .await
        .map_err(transport)?;

    let session: serde_json::Value = get_json(client, &format!("{base}/api/auth/session")).await?;
    if session.get("user").is_some_and(|user| !user.is_null()) {
        Ok(())
    } else {
        Err(DraftError::Unauthorized)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Csrf {
    csrf_token: String,
}

async fn get_json<T: serde::de::DeserializeOwned>(client: &Client, url: &str) -> Result<T, DraftError> {
    client
        .get(url)
        .send()
        .await
        .map_err(transport)?
        .json::<T>()
        .await
        .map_err(transport)
}

/// A 400 from match creation is `validatePreset` refusing the preset, and its
/// `issues` are the only thing that says which rule was broken.
async fn preset_rejected(response: Response) -> DraftError {
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        issues: Vec<PresetIssue>,
    }

    match response.json::<Body>().await {
        Ok(body) => DraftError::PresetRejected { issues: body.issues },
        Err(err) => DraftError::Transport(format!("the draft tool rejected the preset unreadably: {err}")),
    }
}

fn transport(err: reqwest::Error) -> DraftError {
    DraftError::Transport(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::{Credentials, DraftError, Preset, create_match_at, credentials_from};
    use reqwest::Client;
    use serde_json::json;
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
        assert_eq!(preset.config.options.best_of % 2, 1, "bestOf should be odd");
    }

    #[tokio::test]
    #[ignore = "hits the live draft tool API"]
    async fn an_unknown_preset_is_none_rather_than_an_error() {
        assert!(super::fetch_preset("000000000000000000000000").await.is_none());
    }

    // The session and the retry, against a local stub — §10 forbids live calls,
    // and a saved payload cannot show what a *sequence* of requests does.

    fn stub_client() -> Client {
        Client::builder()
            .cookie_store(true)
            .build()
            .expect("the stub client should build")
    }

    fn credentials() -> Credentials {
        credentials_from(Some("aoe4-bot".to_string()), Some("hunter2".to_string()))
            .expect("test credentials should be accepted")
    }

    /// The three endpoints Auth.js v5 answers during a credentials sign-in.
    /// `signed_in` is what the session probe reports afterwards.
    async fn mount_auth(server: &MockServer, signed_in: bool) {
        Mock::given(method("GET"))
            .and(path("/api/auth/csrf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "csrfToken": "csrf-token" })))
            .mount(server)
            .await;

        // Note the 200: the callback answers a *wrong* password this way too,
        // which is exactly why the probe below is what decides.
        Mock::given(method("POST"))
            .and(path("/api/auth/callback/credentials"))
            .respond_with(
                ResponseTemplate::new(200).insert_header("set-cookie", "authjs.session-token=session; Path=/"),
            )
            .mount(server)
            .await;

        let session = if signed_in {
            json!({ "user": { "id": "65f0", "name": "aoe4-bot" } })
        } else {
            json!({})
        };
        Mock::given(method("GET"))
            .and(path("/api/auth/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(session))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn an_expired_session_signs_in_and_retries_carrying_the_cookie() {
        let server = MockServer::start().await;
        mount_auth(&server, true).await;

        // The first attempt has no cookie yet and is refused; priority makes the
        // order deterministic rather than relying on registration order.
        Mock::given(method("POST"))
            .and(path("/api/matches"))
            .respond_with(ResponseTemplate::new(401))
            .up_to_n_times(1)
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;

        // `header_exists("cookie")` is the assertion that the store worked: this
        // only matches once the session cookie is being sent back.
        Mock::given(method("POST"))
            .and(path("/api/matches"))
            .and(header_exists("cookie"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "65f1", "shareCode": "a1b2c3d4" })))
            .with_priority(2)
            .expect(1)
            .mount(&server)
            .await;

        let created = create_match_at(&stub_client(), &server.uri(), &credentials(), "preset-id")
            .await
            .expect("the retry should succeed");
        assert_eq!(created.id, "65f1");
    }

    #[tokio::test]
    async fn a_bad_password_fails_even_though_the_callback_answers_200() {
        // The regression the session probe exists for: Auth.js redirects a wrong
        // password back to the sign-in page, which a following client sees as OK.
        let server = MockServer::start().await;
        mount_auth(&server, false).await;
        Mock::given(method("POST"))
            .and(path("/api/matches"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = create_match_at(&stub_client(), &server.uri(), &credentials(), "preset-id")
            .await
            .expect_err("a failed sign-in must not look like success");
        assert!(matches!(err, DraftError::Unauthorized), "{err:?}");
    }

    #[tokio::test]
    async fn a_second_401_gives_up_rather_than_looping() {
        let server = MockServer::start().await;
        mount_auth(&server, true).await;
        // `expect(2)` is the point of the test: one original and one retry, never
        // a third. Verified when the server drops.
        Mock::given(method("POST"))
            .and(path("/api/matches"))
            .respond_with(ResponseTemplate::new(401))
            .expect(2)
            .mount(&server)
            .await;

        let err = create_match_at(&stub_client(), &server.uri(), &credentials(), "preset-id")
            .await
            .expect_err("a persistent 401 should fail");
        assert!(matches!(err, DraftError::Unauthorized), "{err:?}");
    }

    #[tokio::test]
    async fn a_rejected_preset_keeps_the_tools_issues() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/matches"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "Preset is not valid for play",
                "issues": [{ "code": "notEnoughGames", "params": { "need": 5, "have": 3 } }]
            })))
            .mount(&server)
            .await;

        let err = create_match_at(&stub_client(), &server.uri(), &credentials(), "preset-id")
            .await
            .expect_err("a 400 is a rejection, not a success");
        let DraftError::PresetRejected { issues } = err else {
            panic!("expected the preset to be rejected, got {err:?}");
        };
        assert_eq!(issues[0].code, "notEnoughGames");
        // The params are what make an issue actionable — "needs 5, has 3" is the
        // whole message, since the tool leaves the wording to its own i18n.
        assert_eq!(issues[0].params.as_ref().expect("params should survive")["need"], 5);
    }

    #[tokio::test]
    async fn someone_elses_private_preset_is_forbidden_not_a_transport_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/matches"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let err = create_match_at(&stub_client(), &server.uri(), &credentials(), "preset-id")
            .await
            .expect_err("403 is a refusal");
        assert!(matches!(err, DraftError::Forbidden), "{err:?}");
    }

    #[tokio::test]
    #[ignore = "hits the live draft tool API"]
    async fn the_live_handshake_still_signs_in() {
        // What the stub cannot prove: that Auth.js still answers this flow, and
        // that our reading of `auth.ts` matches the running instance. Needs
        // DRAFT_USERNAME and DRAFT_PASSWORD in the environment.
        let credentials = Credentials::from_env().expect("set DRAFT_USERNAME and DRAFT_PASSWORD");
        super::sign_in_at(&stub_client(), &super::base_url(), &credentials)
            .await
            .expect("the live handshake should sign in");
    }

    #[tokio::test]
    #[ignore = "hits the live draft tool API, and leaves a room behind"]
    async fn the_live_endpoint_opens_a_room() {
        let credentials = Credentials::from_env().expect("set DRAFT_USERNAME and DRAFT_PASSWORD");
        let created = create_match_at(
            &stub_client(),
            &super::base_url(),
            &credentials,
            "6a4325f095d3637a0d064c21",
        )
        .await
        .expect("a public preset should open a room");
        assert!(!created.id.is_empty());
    }

    #[test]
    fn credentials_must_be_present_and_not_blank() {
        // A deployment with no draft credentials loses draft creation and keeps
        // everything else, so this is a clean answer rather than a panic.
        assert!(matches!(credentials_from(None, None), Err(DraftError::NotConfigured)));
        assert!(matches!(
            credentials_from(Some("aoe4-bot".to_string()), None),
            Err(DraftError::NotConfigured)
        ));
        assert!(
            matches!(
                credentials_from(Some("   ".to_string()), Some("hunter2".to_string())),
                Err(DraftError::NotConfigured)
            ),
            "a blank username is not configuration"
        );
        assert!(credentials_from(Some("aoe4-bot".to_string()), Some("hunter2".to_string())).is_ok());
    }
}
