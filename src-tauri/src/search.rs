use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::state::{AppState, lock};

#[derive(Clone)]
pub struct SearchAuth {
    pub username: String,
    pub password: String,
    pub schema_version: u32,
    pub channels: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageHit {
    pub attr_name: String,
    pub version: String,
    pub description: String,
}

pub struct DiscoveredSearch {
    pub schema_version: u32,
    pub username: String,
    pub password: String,
    pub channels: Vec<String>,
}

fn script_src_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)<script[^>]*\ssrc\s*=\s*["']([^"']+)["']"#).unwrap())
}

pub fn script_urls(html: &str, base: &str) -> Vec<String> {
    let base = base.trim_end_matches('/');
    script_src_re()
        .captures_iter(html)
        .filter_map(|caps| {
            let src = caps.get(1)?.as_str();
            if !src.contains(".js") {
                return None;
            }
            let url = if src.starts_with("https://") || src.starts_with("http://") {
                src.to_string()
            } else if let Some(rest) = src.strip_prefix("//") {
                format!("https://{rest}")
            } else if let Some(rest) = src.strip_prefix('/') {
                format!("{base}/{rest}")
            } else {
                format!("{base}/{src}")
            };
            Some(url)
        })
        .collect()
}

fn js_string_at(source: &str, key: &str) -> Option<String> {
    let marker = format!("{key}:");
    let start = source.find(&marker)?;
    let rest = source[start + marker.len()..].trim_start();
    let bytes = rest.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                if index + 1 >= bytes.len() {
                    return None;
                }
                out.push(bytes[index + 1] as char);
                index += 2;
            }
            b'"' => return Some(out),
            byte => {
                out.push(byte as char);
                index += 1;
            }
        }
    }
    None
}

fn json_parse_at(source: &str, key: &str) -> Option<String> {
    let marker = format!("{key}:JSON.parse('");
    let start = source.find(&marker)?;
    let rest = &source[start + marker.len()..];
    let bytes = rest.as_bytes();
    let mut out = String::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                if index + 1 >= bytes.len() {
                    return None;
                }
                out.push(bytes[index + 1] as char);
                index += 2;
            }
            b'\'' => return Some(out),
            byte => {
                out.push(byte as char);
                index += 1;
            }
        }
    }
    None
}

pub fn parse_search_bundle(js: &str) -> Result<DiscoveredSearch, String> {
    let marker = "Elm.Main.init";
    let Some(index) = js.find(marker) else {
        return Err("Elm.Main.init not found in the search.nixos.org bundle".into());
    };
    let window = &js[index..];
    let window = &window[..window.len().min(20_000)];

    let schema_raw = window
        .split("elasticsearchMappingSchemaVersion:")
        .nth(1)
        .ok_or("elasticsearchMappingSchemaVersion was not in Elm.Main.init flags")?;
    let schema_digits: String = schema_raw
        .trim_start()
        .trim_start_matches("parseInt(")
        .trim_start()
        .trim_start_matches('"')
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if schema_digits.is_empty() {
        return Err("could not read elasticsearchMappingSchemaVersion".into());
    }
    let schema_version: u32 = schema_digits
        .parse()
        .map_err(|_| format!("invalid schema version '{schema_digits}'"))?;

    let username = js_string_at(window, "elasticsearchUsername")
        .ok_or("elasticsearchUsername was not in Elm.Main.init flags")?;
    let password = js_string_at(window, "elasticsearchPassword")
        .ok_or("elasticsearchPassword was not in Elm.Main.init flags")?;
    if username.is_empty() || password.is_empty() {
        return Err("search.nixos.org bundle had empty elasticsearch credentials".into());
    }

    let channels_json = json_parse_at(window, "nixosChannels")
        .ok_or("nixosChannels was not in Elm.Main.init flags")?;
    let parsed: Value = serde_json::from_str(&channels_json)
        .map_err(|err| format!("could not parse nixosChannels JSON: {err}"))?;
    let mut channels = Vec::new();
    let entries = parsed
        .get("channels")
        .and_then(|value| value.as_array())
        .ok_or("nixosChannels JSON has no channels array")?;
    for entry in entries {
        if let Some(id) = entry.get("id").and_then(|value| value.as_str()) {
            if !id.is_empty() {
                channels.push(id.to_string());
            }
        }
    }
    if channels.is_empty() {
        return Err("search.nixos.org bundle listed no channels".into());
    }

    Ok(DiscoveredSearch {
        schema_version,
        username,
        password,
        channels,
    })
}

pub fn parse_hits(value: &Value) -> Result<Vec<PackageHit>, String> {
    let hits = value
        .pointer("/hits/hits")
        .and_then(|hits| hits.as_array())
        .ok_or("search response did not contain hits.hits")?;
    let mut packages = Vec::new();
    for hit in hits {
        let Some(source) = hit.get("_source") else {
            continue;
        };
        let Some(attr_name) = source
            .get("package_attr_name")
            .and_then(|value| value.as_str())
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let version = source
            .get("package_pversion")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let description = source
            .get("package_description")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        packages.push(PackageHit {
            attr_name: attr_name.to_string(),
            version,
            description,
        });
    }
    Ok(packages)
}

async fn load_search_auth(app: &AppHandle) -> Result<SearchAuth, String> {
    let http = {
        let state = app.state::<AppState>();
        let cached = lock(&state.inner).search.clone();
        if let Some(cached) = cached {
            return Ok(cached);
        }
        state.http.clone()
    };

    let page = http
        .get("https://search.nixos.org/packages")
        .send()
        .await
        .map_err(|err| format!("failed to fetch https://search.nixos.org/packages: {err}"))?;
    let status = page.status();
    if !status.is_success() {
        return Err(format!(
            "https://search.nixos.org/packages returned HTTP {status}"
        ));
    }
    let html = page
        .text()
        .await
        .map_err(|err| format!("failed to read the search.nixos.org page: {err}"))?;
    let urls = script_urls(&html, "https://search.nixos.org");
    if urls.is_empty() {
        return Err("search.nixos.org/packages did not link a JavaScript bundle".into());
    }

    let mut last_error = "bundle did not contain Elm.Main.init flags".to_string();
    for url in urls {
        let response = match http.get(&url).send().await {
            Ok(response) => response,
            Err(err) => {
                last_error = format!("failed to fetch {url}: {err}");
                continue;
            }
        };
        if !response.status().is_success() {
            last_error = format!("{url} returned HTTP {}", response.status());
            continue;
        }
        let js = match response.text().await {
            Ok(js) => js,
            Err(err) => {
                last_error = format!("failed to read {url}: {err}");
                continue;
            }
        };
        match parse_search_bundle(&js) {
            Ok(found) => {
                let auth = SearchAuth {
                    username: found.username,
                    password: found.password,
                    schema_version: found.schema_version,
                    channels: found.channels,
                };
                crate::state::with_inner(app, |inner| inner.search = Some(auth.clone()));
                return Ok(auth);
            }
            Err(err) => last_error = err,
        }
    }

    Err(format!(
        "could not read search.nixos.org settings from the public bundle: {last_error}"
    ))
}

#[tauri::command]
pub async fn search_packages(app: AppHandle, query: String) -> Result<Vec<PackageHit>, String> {
    let query = query.trim();
    if query.chars().count() < 2 {
        return Ok(Vec::new());
    }
    if query.len() > 200 {
        return Err("search query is too long".into());
    }

    let auth = load_search_auth(&app).await?;
    if !auth.channels.iter().any(|channel| channel == "unstable") {
        return Err(format!(
            "search.nixos.org has no unstable channel (found: {})",
            auth.channels.join(", ")
        ));
    }

    let http = app.state::<AppState>().http.clone();
    let url = format!(
        "https://search.nixos.org/backend/latest-{}-nixos-unstable/_search",
        auth.schema_version
    );
    let body = serde_json::json!({
        "size": 20,
        "query": {
            "bool": {
                "filter": [{ "term": { "type": "package" } }],
                "must": [{
                    "multi_match": {
                        "query": query,
                        "fields": [
                            "package_attr_name^4",
                            "package_pname^2",
                            "package_description"
                        ]
                    }
                }]
            }
        }
    });

    let response = http
        .post(url)
        .basic_auth(&auth.username, Some(&auth.password))
        .json(&body)
        .send()
        .await
        .map_err(|err| format!("package search request failed: {err}"))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let mut text: String = text.chars().take(400).collect();
        if text.chars().count() == 400 {
            text.push_str("...");
        }
        return Err(format!("package search returned HTTP {status}: {text}"));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|err| format!("package search response was not JSON: {err}"))?;
    parse_hits(&payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
        var other = "Elm is unrelated";
        f = t.Elm.Main.init({flags:{elasticsearchMappingSchemaVersion:parseInt("52"),elasticsearchUrl:"/backend",elasticsearchUsername:"user",elasticsearchPassword:"s3cret\"x",nixosChannels:JSON.parse('{"channels":[{"branch":"nixos-unstable","id":"unstable","status":"rolling"},{"id":"26.05"}],"default":"26.05"}'),theme:"auto"}});
    "#;

    #[test]
    fn parses_elm_init_flags() {
        let found = parse_search_bundle(FIXTURE).unwrap();
        assert_eq!(found.schema_version, 52);
        assert_eq!(found.username, "user");
        assert_eq!(found.password, "s3cret\"x");
        assert_eq!(
            found.channels,
            vec!["unstable".to_string(), "26.05".to_string()]
        );
    }

    #[test]
    fn reads_script_urls_from_html() {
        let html = r#"<html><script defer src="/static/js/index.abc.js"></script><script src="https://cdn.example/skip.js"></script></html>"#;
        let urls = script_urls(html, "https://search.nixos.org");
        assert_eq!(
            urls,
            vec![
                "https://search.nixos.org/static/js/index.abc.js".to_string(),
                "https://cdn.example/skip.js".to_string(),
            ]
        );
    }

    #[test]
    fn parses_package_hits() {
        let value = serde_json::json!({
            "hits": {
                "hits": [
                    {
                        "_source": {
                            "package_attr_name": "git",
                            "package_pversion": "2.56.0",
                            "package_description": "Distributed version control system"
                        }
                    },
                    {
                        "_source": {
                            "package_attr_name": "emacsPackages.git",
                            "package_pversion": "1",
                            "package_description": null
                        }
                    },
                    { "_source": { "package_pversion": "0" } }
                ]
            }
        });
        let hits = parse_hits(&value).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].attr_name, "git");
        assert_eq!(hits[0].version, "2.56.0");
        assert_eq!(hits[1].description, "");
    }

    #[test]
    fn missing_hits_is_an_error() {
        let err = parse_hits(&serde_json::json!({"ok": true})).unwrap_err();
        assert!(err.contains("hits"));
    }

    #[tokio::test]
    #[ignore = "talks to search.nixos.org"]
    async fn live_unstable_search_returns_real_packages() {
        let http = reqwest::Client::builder()
            .user_agent("nix-pi-flasher")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("client");
        let html = http
            .get("https://search.nixos.org/packages")
            .send()
            .await
            .expect("page")
            .error_for_status()
            .expect("page status")
            .text()
            .await
            .expect("page body");
        let mut discovered = None;
        for url in script_urls(&html, "https://search.nixos.org") {
            let js = match http.get(&url).send().await {
                Ok(response) => response.text().await.unwrap_or_default(),
                Err(_) => continue,
            };
            if let Ok(parsed) = parse_search_bundle(&js) {
                discovered = Some(parsed);
                break;
            }
        }
        let discovered = discovered.expect("Elm.Main.init flags");
        assert!(discovered.schema_version > 0);
        assert!(
            discovered
                .channels
                .iter()
                .any(|channel| channel == "unstable"),
            "channels: {:?}",
            discovered.channels
        );
        let url = format!(
            "https://search.nixos.org/backend/latest-{}-nixos-unstable/_search",
            discovered.schema_version
        );
        let body = serde_json::json!({
            "size": 5,
            "query": {
                "bool": {
                    "filter": [{ "term": { "type": "package" } }],
                    "must": [{
                        "multi_match": {
                            "query": "git",
                            "fields": [
                                "package_attr_name^4",
                                "package_pname^2",
                                "package_description"
                            ]
                        }
                    }]
                }
            }
        });
        let payload = http
            .post(url)
            .basic_auth(discovered.username, Some(discovered.password))
            .json(&body)
            .send()
            .await
            .expect("search")
            .error_for_status()
            .expect("search status")
            .json::<serde_json::Value>()
            .await
            .expect("search json");
        let hits = parse_hits(&payload).expect("hits");
        assert!(
            hits.iter().any(|hit| hit.attr_name == "git"),
            "expected git among {:?}",
            hits.iter()
                .map(|hit| hit.attr_name.as_str())
                .collect::<Vec<_>>()
        );
    }
}
