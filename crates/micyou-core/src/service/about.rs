/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 * Derived from MicYou <https://github.com/LanRhyme/MicYou>.
 *
 * Copyright (C) 2026 LanRhyme (original MicYou)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version, with the MicYou Plugin Exception.
 * See LICENSE for details.
 */

//! System services of the [`Backend`]: update checks (GitHub releases with a
//! MirrorChyan CDK channel) and the sponsor list proxy (RPC `system/*`).

use micyou_api::methods::{SponsorsParams, SponsorsResult, UpdateCheckParams, UpdateCheckResult};
use reqwest::Client;

use crate::service::Backend;

fn mirror_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "win"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn mirror_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    }
}

#[derive(serde::Deserialize)]
struct MirrorChyanResponse {
    code: i32,
    #[allow(dead_code)]
    msg: Option<String>,
    data: Option<MirrorChyanData>,
}

#[derive(serde::Deserialize)]
struct MirrorChyanData {
    version_name: Option<String>,
    #[allow(dead_code)]
    version_number: Option<i64>,
    release_note: Option<String>,
    url: Option<String>,
    cdk_expired_time: Option<i64>,
}

impl Backend {
    /// Check for a newer MicYou release.
    ///
    /// Order: MirrorChyan mirror (when a CDK is supplied) → GitHub releases
    /// API → GitHub `releases/latest` redirect (avoids the unauthenticated
    /// 60 req/hr API rate limit).
    pub async fn update_check(
        &self,
        params: UpdateCheckParams,
    ) -> Result<UpdateCheckResult, String> {
        let current_version = env!("CARGO_PKG_VERSION");
        let current_semver = semver::Version::parse(current_version)
            .unwrap_or_else(|_| semver::Version::new(0, 0, 0));

        let client = Client::builder()
            .user_agent(format!("libmicyou/{}", current_version))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;

        // 1. MirrorChyan (CDK holders get the mirrored download channel)
        if let Some(cdk_str) = params
            .cdk
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            let mirror_url = format!(
                "https://mirrorchyan.com/api/resources/MicYou/latest?os={}&arch={}&cdk={}",
                mirror_os(),
                mirror_arch(),
                cdk_str
            );

            if let Ok(res) = client.get(&mirror_url).send().await {
                if res.status().is_success() {
                    if let Ok(resp) = res.json::<MirrorChyanResponse>().await {
                        if resp.code == 0 {
                            if let Some(data) = resp.data {
                                if let Some(download_url) = data.url.filter(|u| !u.is_empty()) {
                                    let latest_version = data
                                        .version_name
                                        .as_deref()
                                        .unwrap_or("")
                                        .trim_start_matches('v')
                                        .to_string();
                                    let latest_semver = semver::Version::parse(&latest_version)
                                        .unwrap_or_else(|_| semver::Version::new(0, 0, 0));
                                    return Ok(UpdateCheckResult {
                                        has_update: latest_semver > current_semver,
                                        current_version: current_version.to_string(),
                                        latest_version,
                                        release_url: download_url,
                                        release_notes: data.release_note,
                                        is_mirror: true,
                                        cdk_expired_time: data.cdk_expired_time,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. GitHub releases API (includes changelog notes when available)
        let api_res = client
            .get("https://api.github.com/repos/LanRhyme/MicYou/releases/latest")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await;

        if let Ok(res) = api_res {
            if res.status().is_success() {
                if let Ok(json) = res.json::<serde_json::Value>().await {
                    if let Some(tag) = json.get("tag_name").and_then(|t| t.as_str()) {
                        let latest_version = tag.trim_start_matches('v').to_string();
                        let latest_semver = semver::Version::parse(&latest_version)
                            .unwrap_or_else(|_| semver::Version::new(0, 0, 0));
                        let release_url = json
                            .get("html_url")
                            .and_then(|u| u.as_str())
                            .unwrap_or("https://github.com/LanRhyme/MicYou/releases/latest")
                            .to_string();
                        let release_notes = json
                            .get("body")
                            .and_then(|b| b.as_str())
                            .map(|s| s.to_string());

                        return Ok(UpdateCheckResult {
                            has_update: latest_semver > current_semver,
                            current_version: current_version.to_string(),
                            latest_version,
                            release_url,
                            release_notes,
                            is_mirror: false,
                            cdk_expired_time: None,
                        });
                    }
                }
            }
        }

        // 3. Fallback: follow the releases/latest redirect and parse the tag
        let web_res = client
            .get("https://github.com/LanRhyme/MicYou/releases/latest")
            .send()
            .await
            .map_err(|e| format!("network request failed: {e}"))?;

        let final_url = web_res.url().as_str();
        if let Some(tag) = final_url.split("/tag/").nth(1) {
            let tag = tag.trim_matches('/');
            let latest_version = tag.trim_start_matches('v').to_string();
            let latest_semver = semver::Version::parse(&latest_version)
                .unwrap_or_else(|_| semver::Version::new(0, 0, 0));

            return Ok(UpdateCheckResult {
                has_update: latest_semver > current_semver,
                current_version: current_version.to_string(),
                latest_version,
                release_url: final_url.to_string(),
                release_notes: None,
                is_mirror: false,
                cdk_expired_time: None,
            });
        }

        Err("could not determine the latest version".to_string())
    }

    /// Proxy the Afdian sponsor query. Credentials come from the request or
    /// the `AIFADIAN_API_TOKEN` / `AIFADIAN_USER_ID` environment variables.
    pub async fn sponsors(&self, params: SponsorsParams) -> Result<SponsorsResult, String> {
        use md5::Digest;

        let api_token = params
            .api_token
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("AIFADIAN_API_TOKEN").ok())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| "API not configured".to_string())?;
        let user_id = params
            .user_id
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("AIFADIAN_USER_ID").ok())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| "API not configured".to_string())?;

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let query_params = r#"{"page":"1","per_page":"100"}"#;
        let sign_str = format!(
            "{}params{}ts{}user_id{}",
            api_token, query_params, ts, user_id
        );
        let mut hasher = md5::Md5::new();
        md5::Digest::update(&mut hasher, sign_str.as_bytes());
        let digest = hasher.finalize();
        let sign = digest
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>();

        let client = Client::new();
        let req_body = serde_json::json!({
            "user_id": user_id,
            "params": query_params,
            "ts": ts,
            "sign": sign
        });

        let res = client
            .post("https://afdian.com/api/open/query-sponsor")
            .json(&req_body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let text = res.text().await.map_err(|e| e.to_string())?;
        Ok(SponsorsResult { raw: text })
    }
}

#[cfg(test)]
mod tests {
    use super::{mirror_arch, mirror_os};

    #[test]
    fn mirror_identifiers_are_valid() {
        assert!(["win", "macos", "linux"].contains(&mirror_os()));
        assert!(["x64", "arm64"].contains(&mirror_arch()));
    }
}
