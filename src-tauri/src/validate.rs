use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

const MAX_PACKAGES: usize = 50;
const MAX_PACKAGE_LEN: usize = 256;
const MAX_PASSWORD_LEN: usize = 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildSettings {
    pub set_hostname: bool,
    pub hostname: String,
    pub enable_ssh: bool,
    pub create_user: bool,
    pub username: String,
    pub password: String,
    pub packages: Vec<String>,
}

/// JSON written for the flake. The plaintext password is never a field.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FlasherConfig {
    pub set_hostname: bool,
    pub hostname: String,
    pub enable_ssh: bool,
    pub create_user: bool,
    pub username: String,
    pub hashed_password: Option<String>,
    pub packages: Vec<String>,
}

fn hostname_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?$").unwrap())
}

fn username_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z_][a-z0-9_-]{0,31}$").unwrap())
}

fn package_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^[A-Za-z_][A-Za-z0-9_+.-]*(\.[A-Za-z_][A-Za-z0-9_+.-]*)*$").unwrap()
    })
}

fn preview(value: &str) -> String {
    let mut chars = value.chars();
    let short: String = chars.by_ref().take(80).collect();
    if chars.next().is_some() {
        format!("{short}...")
    } else {
        short
    }
}

pub fn hash_password(password: &str) -> Result<String, String> {
    pwhash::sha512_crypt::hash(password).map_err(|err| format!("failed to hash password: {err}"))
}

pub fn validate_settings(settings: &BuildSettings) -> Result<FlasherConfig, String> {
    if settings.set_hostname {
        if !hostname_re().is_match(&settings.hostname) {
            return Err(format!(
                "hostname '{}' is invalid. Use 1-63 letters, digits, or hyphens, and do not start or end with a hyphen.",
                preview(&settings.hostname)
            ));
        }
    } else if !settings.hostname.is_empty() && !hostname_re().is_match(&settings.hostname) {
        return Err(format!(
            "hostname '{}' is invalid",
            preview(&settings.hostname)
        ));
    }

    let username = if settings.username.is_empty() && !settings.create_user {
        "nixos".to_string()
    } else {
        settings.username.clone()
    };
    if !username_re().is_match(&username) {
        return Err(format!(
            "username '{}' is invalid. Use 1-32 characters: a lowercase letter or underscore, then lowercase letters, digits, underscores, or hyphens.",
            preview(&username)
        ));
    }

    if settings.packages.len() > MAX_PACKAGES {
        return Err(format!(
            "too many packages ({}). The maximum is {MAX_PACKAGES}.",
            settings.packages.len()
        ));
    }

    let mut packages = Vec::with_capacity(settings.packages.len());
    let mut seen = std::collections::HashSet::new();
    for package in &settings.packages {
        if package.len() > MAX_PACKAGE_LEN {
            return Err(format!(
                "package attribute is too long (max {MAX_PACKAGE_LEN}): {}",
                preview(package)
            ));
        }
        if !package_re().is_match(package) {
            return Err(format!(
                "invalid package attribute '{}'. Use a nixpkgs attribute path such as git or python3Packages.requests.",
                preview(package)
            ));
        }
        if seen.insert(package.clone()) {
            packages.push(package.clone());
        }
    }

    let hashed_password = if settings.create_user {
        if settings.password.is_empty() {
            return Err(
                "createUser requires a password. It is hashed locally and only the hash is written."
                    .into(),
            );
        }
        if settings.password.len() > MAX_PASSWORD_LEN {
            return Err(format!(
                "password is too long (max {MAX_PASSWORD_LEN} bytes)"
            ));
        }
        let hash = hash_password(&settings.password)?;
        if !hash.starts_with("$6$") {
            return Err("password hash was not a SHA-512 crypt ($6$)".into());
        }
        Some(hash)
    } else {
        None
    };

    Ok(FlasherConfig {
        set_hostname: settings.set_hostname,
        hostname: if settings.hostname.is_empty() {
            "nixpi".to_string()
        } else {
            settings.hostname.clone()
        },
        enable_ssh: settings.enable_ssh,
        create_user: settings.create_user,
        username,
        hashed_password,
        packages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> BuildSettings {
        BuildSettings {
            set_hostname: true,
            hostname: "nixpi".into(),
            enable_ssh: true,
            create_user: false,
            username: "nixos".into(),
            password: String::new(),
            packages: vec![],
        }
    }

    #[test]
    fn accepts_good_names() {
        let mut settings = base();
        settings.hostname = "pi-4".into();
        settings.username = "_svc".into();
        settings.packages = vec!["git".into(), "python3Packages.requests".into()];
        let config = validate_settings(&settings).unwrap();
        assert_eq!(config.hostname, "pi-4");
        assert_eq!(config.username, "_svc");
        assert_eq!(
            config.packages,
            vec!["git".to_string(), "python3Packages.requests".to_string()]
        );
        assert!(config.hashed_password.is_none());
        let single = base();
        let mut one = single;
        one.hostname = "A".into();
        one.username = "a".into();
        assert!(validate_settings(&one).is_ok());
    }

    #[test]
    fn rejects_injection_and_traversal() {
        for bad in ["foo;bar", "../x", "a b", "foo|bar", "-bad"] {
            let mut settings = base();
            settings.hostname = bad.into();
            assert!(validate_settings(&settings).is_err(), "hostname {bad}");

            let mut settings = base();
            settings.username = bad.into();
            assert!(validate_settings(&settings).is_err(), "username {bad}");

            let mut settings = base();
            settings.packages = vec![bad.into()];
            assert!(validate_settings(&settings).is_err(), "package {bad}");
        }

        let mut settings = base();
        settings.hostname = "bad-".into();
        assert!(
            validate_settings(&settings).is_err(),
            "hostname cannot end with a hyphen"
        );
    }

    #[test]
    fn rejects_empty_password_when_creating_user() {
        let mut settings = base();
        settings.create_user = true;
        settings.password.clear();
        let err = validate_settings(&settings).unwrap_err();
        assert!(err.to_lowercase().contains("password"));
    }

    #[test]
    fn hash_starts_with_sha512_crypt_and_is_not_plaintext() {
        let mut settings = base();
        settings.create_user = true;
        settings.password = "s3cret-pass".into();
        let config = validate_settings(&settings).unwrap();
        let hash = config.hashed_password.clone().unwrap();
        assert!(hash.starts_with("$6$"), "{hash}");
        assert!(!hash.contains("s3cret-pass"));
        assert!(pwhash::sha512_crypt::verify("s3cret-pass", &hash));
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("s3cret-pass"));
        assert!(json.contains("hashedPassword"));
        assert!(!json.contains("\"password\""));
    }

    #[test]
    fn json_fields_match_the_schema() {
        let config = validate_settings(&base()).unwrap();
        let value = serde_json::to_value(&config).unwrap();
        for key in [
            "setHostname",
            "hostname",
            "enableSsh",
            "createUser",
            "username",
            "hashedPassword",
            "packages",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        assert!(value["hashedPassword"].is_null());
        assert_eq!(value["packages"], serde_json::json!([]));
        assert_eq!(value["hostname"], "nixpi");
    }

    #[test]
    fn rejects_too_many_packages_and_dedupes() {
        let mut settings = base();
        settings.packages = (0..51).map(|n| format!("pkg{n}")).collect();
        assert!(validate_settings(&settings).is_err());

        settings.packages = vec!["git".into(), "git".into(), "vim".into()];
        let config = validate_settings(&settings).unwrap();
        assert_eq!(config.packages, vec!["git".to_string(), "vim".to_string()]);
    }

    #[test]
    fn skips_hostname_check_when_unset_and_empty() {
        let mut settings = base();
        settings.set_hostname = false;
        settings.hostname.clear();
        let config = validate_settings(&settings).unwrap();
        assert!(!config.set_hostname);
        assert_eq!(config.hostname, "nixpi");
    }
}
