//! Steam Workshop **metadata** via the Steam Web API.
//!
//! - `ISteamRemoteStorage/GetPublishedFileDetails/v1/` is **keyless** and returns
//!   title, description, preview URL, size, subscriptions, views, tags, etc.
//! - `IPublishedFileService/QueryFiles/v1/` (browse/search) requires a free
//!   Web API key (`STEAM_WEB_API_KEY`).
//!
//! It cannot download content: for content-managed items `file_url` is empty and
//! the bytes are delivered by the Steam client (or SteamCMD / the SDK). Point
//! [`crate::workshop::LocalWorkshopSource`] at the downloaded content instead.
//!
//! Response parsing is pure and always available; the HTTP client is behind the
//! `workshop-web` feature.

use serde::{Deserialize, Deserializer, Serialize};

use crate::{Error, Result};

/// Steam sends the same field as `0`/`1` on one endpoint and `false`/`true` on another —
/// `GetPublishedFileDetails` reports `banned: 0`, `QueryFiles` reports `banned: false` — so the
/// integer fields that can arrive either way are read through this.
fn int_or_bool<'de, D>(deserializer: D) -> std::result::Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Int(i64),
        Bool(bool),
    }

    Ok(match Either::deserialize(deserializer)? {
        Either::Int(value) => value,
        Either::Bool(value) => i64::from(value),
    })
}

/// One workshop tag.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tag {
    pub tag: String,
}

/// A published-file record as returned by the Web API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PublishedFileDetails {
    #[serde(rename = "publishedfileid")]
    pub published_file_id: String,
    #[serde(default, deserialize_with = "int_or_bool")]
    pub result: i64,
    pub creator: String,
    pub creator_app_id: u64,
    pub consumer_app_id: u64,
    pub filename: String,
    pub file_size: String,
    pub file_url: String,
    pub hcontent_file: String,
    pub preview_url: String,
    pub hcontent_preview: String,
    pub title: String,
    pub description: String,
    pub time_created: u64,
    pub time_updated: u64,
    #[serde(default, deserialize_with = "int_or_bool")]
    pub visibility: i64,
    #[serde(default, deserialize_with = "int_or_bool")]
    pub banned: i64,
    pub ban_reason: String,
    pub subscriptions: u64,
    pub favorited: u64,
    pub lifetime_subscriptions: u64,
    pub lifetime_favorited: u64,
    pub views: u64,
    pub tags: Vec<Tag>,
}

impl PublishedFileDetails {
    /// The numeric published file id.
    pub fn id(&self) -> Option<u64> {
        self.published_file_id.parse().ok()
    }

    /// `file_size` as a number (the API encodes it as a string).
    pub fn file_size_bytes(&self) -> Option<u64> {
        self.file_size.parse().ok()
    }

    pub fn tag_names(&self) -> impl Iterator<Item = &str> {
        self.tags.iter().map(|tag| tag.tag.as_str())
    }

    /// Whether the item has a direct download URL (legacy single-file UGC).
    pub fn has_direct_url(&self) -> bool {
        !self.file_url.is_empty()
    }
}

#[derive(Deserialize)]
struct DetailsEnvelope {
    response: DetailsBody,
}

#[derive(Deserialize)]
struct DetailsBody {
    #[serde(default)]
    publishedfiledetails: Vec<PublishedFileDetails>,
}

/// Parse a `GetPublishedFileDetails` / `QueryFiles` response body.
pub fn parse_published_file_details(json: &str) -> Result<Vec<PublishedFileDetails>> {
    let envelope: DetailsEnvelope =
        serde_json::from_str(json).map_err(|err| Error::Parse(err.to_string()))?;
    Ok(envelope.response.publishedfiledetails)
}

/// One page of a `QueryFiles` response.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryPage {
    /// How many items Steam says match the query. Not a promise — it can be rounded for very
    /// broad searches — which is why the window says "about" when it shows it.
    pub total: u64,
    /// What to send as `cursor` for the page after this one. `None` means this is the last page.
    pub next_cursor: Option<String>,
    pub items: Vec<PublishedFileDetails>,
}

#[derive(Deserialize)]
struct QueryEnvelope {
    response: QueryBody,
}

#[derive(Deserialize)]
struct QueryBody {
    #[serde(default)]
    total: u64,
    #[serde(default)]
    next_cursor: String,
    #[serde(default)]
    publishedfiledetails: Vec<PublishedFileDetails>,
}

/// Parse a `QueryFiles` response body, total and cursor included.
///
/// The cursor matters: `page` is capped at 1000 and cannot paginate deeply, which is why Steam's
/// own documentation prefers the cursor. An empty or missing `next_cursor` is the last page.
pub fn parse_query_page(json: &str) -> Result<QueryPage> {
    let envelope: QueryEnvelope =
        serde_json::from_str(json).map_err(|err| Error::Parse(err.to_string()))?;
    let cursor = envelope.response.next_cursor.trim();
    Ok(QueryPage {
        total: envelope.response.total,
        next_cursor: (!cursor.is_empty()).then(|| cursor.to_string()),
        items: envelope.response.publishedfiledetails,
    })
}

/// Build the `application/x-www-form-urlencoded` body for
/// `GetPublishedFileDetails`.
pub fn build_details_form(ids: &[u64]) -> String {
    let mut form = format!("itemcount={}", ids.len());
    for (index, id) in ids.iter().enumerate() {
        form.push_str(&format!("&publishedfileids[{index}]={id}"));
    }
    form
}

/// Where the studio's own settings live (`studio.json` in the user data directory).
pub fn studio_settings_path() -> std::path::PathBuf {
    crate::paths::user_data_dir().join(crate::settings::STUDIO_SETTINGS_FILE)
}

/// The key to use, given what the environment and the settings file each had to say.
///
/// The environment wins: it is a deliberate, temporary override. The file is what makes the window
/// work when it is started from a desktop entry or a Flatpak, where no shell exported anything.
/// Blank on either side counts as absent — an empty variable, or an empty string in the file, is a
/// mistake rather than a key.
pub fn resolve_api_key(env: Option<String>, file: Option<String>) -> Option<String> {
    [env, file]
        .into_iter()
        .flatten()
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

/// Keep this many ids per request (the endpoint caps request size).
pub const MAX_IDS_PER_REQUEST: usize = 100;

#[cfg(feature = "workshop-web")]
mod web {
    use std::time::Duration;

    use super::*;

    /// Steam Web API root.
    pub const DEFAULT_BASE_URL: &str = "https://api.steampowered.com";
    /// Environment variable holding the free Web API key (needed for `QueryFiles`).
    pub const API_KEY_ENV: &str = "STEAM_WEB_API_KEY";

    /// Blocking Steam Web API client for workshop metadata.
    #[derive(Debug)]
    pub struct WorkshopWeb {
        client: reqwest::blocking::Client,
        base_url: String,
        key: Option<String>,
    }

    impl WorkshopWeb {
        /// Build from `STEAM_WEB_API_KEY`, else `studio.json`, and the public API root.
        pub fn new() -> Result<Self> {
            use crate::settings::ConfigFile;

            let file = crate::settings::StudioSettings::load(&studio_settings_path()).steam_api_key;
            let key = resolve_api_key(std::env::var(API_KEY_ENV).ok(), Some(file));
            Self::with_base_url(DEFAULT_BASE_URL, key)
        }

        /// Build against a custom base URL (used by tests).
        pub fn with_base_url(base_url: impl Into<String>, key: Option<String>) -> Result<Self> {
            let client = reqwest::blocking::Client::builder()
                .user_agent(concat!("anima-engine-rs/", env!("CARGO_PKG_VERSION")))
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(http_err)?;
            Ok(Self {
                client,
                base_url: base_url.into(),
                key,
            })
        }

        pub fn has_api_key(&self) -> bool {
            self.key.is_some()
        }

        /// Keyless: fetch metadata for up to [`MAX_IDS_PER_REQUEST`] items.
        pub fn fetch_published_file_details(
            &self,
            ids: &[u64],
        ) -> Result<Vec<PublishedFileDetails>> {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let url = format!(
                "{}/ISteamRemoteStorage/GetPublishedFileDetails/v1/",
                self.base_url
            );
            let body = build_details_form(&ids[..ids.len().min(MAX_IDS_PER_REQUEST)]);
            let text = self
                .client
                .post(url)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(body)
                .send()
                .map_err(http_err)?
                .text()
                .map_err(http_err)?;
            parse_published_file_details(&text)
        }

        /// Browse/search (requires `STEAM_WEB_API_KEY`).
        ///
        /// `cursor` is the `next_cursor` of the previous page; `None` asks for the first page.
        /// Steam's documentation prefers this over `page`, which is capped at 1000 and therefore
        /// cannot paginate deeply.
        pub fn query_files_page(
            &self,
            app_id: u64,
            cursor: Option<&str>,
            num_per_page: u32,
            search_text: Option<&str>,
        ) -> Result<QueryPage> {
            let key = self
                .key
                .as_ref()
                .ok_or_else(|| Error::Http(format!("{API_KEY_ENV} is not set")))?;

            let mut params: Vec<(&str, String)> = vec![
                ("key", key.clone()),
                ("appid", app_id.to_string()),
                ("query_type", "0".to_string()),
                ("cursor", cursor.unwrap_or("*").to_string()),
                ("numperpage", num_per_page.min(100).to_string()),
                ("return_short_description", "true".to_string()),
            ];
            if let Some(text) = search_text.filter(|text| !text.is_empty()) {
                params.push(("search_text", text.to_string()));
            }

            let url = format!("{}/IPublishedFileService/QueryFiles/v1/", self.base_url);
            let text = self
                .client
                .get(url)
                .query(&params)
                .send()
                .map_err(http_err)?
                .text()
                .map_err(http_err)?;
            parse_query_page(&text)
        }
    }

    fn http_err(err: reqwest::Error) -> Error {
        Error::Http(err.to_string())
    }
}

#[cfg(feature = "workshop-web")]
pub use web::{API_KEY_ENV, DEFAULT_BASE_URL, WorkshopWeb};

/// Real captured response, trimmed, used as a parser fixture.
#[cfg(test)]
const SAMPLE_RESPONSE: &str = r#"{"response":{"result":1,"resultcount":2,"publishedfiledetails":[
{"publishedfileid":"3618631640","result":1,"creator":"76561199736546339","creator_app_id":3474900,"consumer_app_id":3474900,"filename":"","file_size":"1282175","file_url":"","hcontent_file":"659556662846826589","preview_url":"https://images.steamusercontent.com/ugc/16376408433248629496/5AEF8B1231FB0C2E8AA2ECEEBAEB66957B5997A8/","hcontent_preview":"16376408433248629496","title":"vocaloid cristmas by azulaDiana","description":"Animation: last cristmas tags:vocaloid,anime,miku,teto,neru,","time_created":1764998743,"time_updated":1764998743,"visibility":0,"banned":0,"ban_reason":"","subscriptions":544,"favorited":8,"lifetime_subscriptions":601,"lifetime_favorited":8,"views":554,"tags":[]},
{"publishedfileid":"3425176851","result":1,"creator":"76561199736546339","creator_app_id":3474900,"consumer_app_id":3474900,"filename":"","file_size":"237871746","file_url":"","hcontent_file":"6840599560970356282","preview_url":"https://images.steamusercontent.com/ugc/41197344872401616/558E4ADE8B49AD7205E04EF0A33C6BF118B50F33/","hcontent_preview":"41197344872401616","title":"anime girl eat your files","description":"MDAA pack","time_created":1739236561,"time_updated":1739236561,"visibility":0,"banned":0,"ban_reason":"","subscriptions":544,"favorited":8,"lifetime_subscriptions":601,"lifetime_favorited":8,"views":554,"tags":[]}
]}}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_captured_response() {
        let items = parse_published_file_details(SAMPLE_RESPONSE).unwrap();
        assert_eq!(items.len(), 2);

        let first = &items[0];
        assert_eq!(first.id(), Some(3_618_631_640));
        assert_eq!(first.title, "vocaloid cristmas by azulaDiana");
        assert_eq!(first.file_size_bytes(), Some(1_282_175));
        assert_eq!(first.subscriptions, 544);
        assert_eq!(first.views, 554);
        assert_eq!(first.consumer_app_id, 3474900);
        assert!(first.preview_url.starts_with("https://"));
        assert!(!first.has_direct_url());

        assert_eq!(items[1].id(), Some(3_425_176_851));
        assert_eq!(items[1].file_size_bytes(), Some(237_871_746));
    }

    #[test]
    fn builds_details_form() {
        assert_eq!(
            build_details_form(&[3618631640, 3425176851]),
            "itemcount=2&publishedfileids[0]=3618631640&publishedfileids[1]=3425176851"
        );
        assert_eq!(build_details_form(&[]), "itemcount=0");
    }

    #[test]
    fn tolerates_missing_fields_and_tags() {
        let json = r#"{"response":{"publishedfiledetails":[{"publishedfileid":"1","title":"t"}]}}"#;
        let items = parse_published_file_details(json).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id(), Some(1));
        assert!(items[0].tags.is_empty());
        assert_eq!(items[0].subscriptions, 0);
    }

    #[test]
    fn rejects_bad_json() {
        assert!(matches!(
            parse_published_file_details("nope"),
            Err(Error::Parse(_))
        ));
    }

    #[test]
    fn parses_tags() {
        let json = r#"{"response":{"publishedfileid":1,"publishedfiledetails":[
            {"publishedfileid":"5","tags":[{"tag":"vocaloid"},{"tag":"anime"}]}
        ]}}"#;
        let items = parse_published_file_details(json).unwrap();
        assert_eq!(
            items[0].tag_names().collect::<Vec<_>>(),
            vec!["vocaloid", "anime"]
        );
    }

    #[cfg(feature = "workshop-web")]
    #[test]
    fn fetch_hits_endpoint_and_parses() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = SAMPLE_RESPONSE.to_string();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let web = WorkshopWeb::with_base_url(format!("http://{address}"), None).unwrap();
        let items = web.fetch_published_file_details(&[3618631640]).unwrap();
        server.join().unwrap();

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "vocaloid cristmas by azulaDiana");
    }

    #[cfg(feature = "workshop-web")]
    #[test]
    fn query_files_without_key_errors() {
        let web = WorkshopWeb::with_base_url("http://127.0.0.1:1", None).unwrap();
        assert!(matches!(
            web.query_files_page(3474900, None, 20, None),
            Err(Error::Http(_))
        ));
    }

    #[test]
    fn the_environment_key_wins_over_the_file() {
        let key = |env: Option<&str>, file: Option<&str>| {
            resolve_api_key(env.map(str::to_string), file.map(str::to_string))
        };
        assert_eq!(key(Some("ENV"), Some("FILE")).as_deref(), Some("ENV"));
        assert_eq!(key(None, Some("FILE")).as_deref(), Some("FILE"));
        assert_eq!(key(None, None), None);
        // Blank is absent, not a key: an empty export must not shadow a good file.
        assert_eq!(key(Some("   "), Some("FILE")).as_deref(), Some("FILE"));
        assert_eq!(key(Some(""), None), None);
        assert_eq!(key(None, Some("  ")), None);
        // The key is trimmed, because a pasted one often carries whitespace.
        assert_eq!(key(Some(" KEY\n"), None).as_deref(), Some("KEY"));
    }

    #[test]
    fn parses_a_page_with_its_total_and_cursor() {
        let json = r#"{"response":{"total":1823,"next_cursor":"AoIIP1gA","publishedfiledetails":[
            {"publishedfileid":"1","title":"first"},
            {"publishedfileid":"2","title":"second"}
        ]}}"#;
        let page = parse_query_page(json).unwrap();
        assert_eq!(page.total, 1823);
        assert_eq!(page.next_cursor.as_deref(), Some("AoIIP1gA"));
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].title, "first");

        // The last page has no cursor to give, and a response without the fields at all is a
        // page of nothing rather than an error.
        let last = parse_query_page(
            r#"{"response":{"total":2,"next_cursor":"","publishedfiledetails":[]}}"#,
        )
        .unwrap();
        assert_eq!(last.next_cursor, None);
        assert!(
            parse_query_page(r#"{"response":{"publishedfiledetails":[]}}"#)
                .unwrap()
                .next_cursor
                .is_none()
        );
    }
}
