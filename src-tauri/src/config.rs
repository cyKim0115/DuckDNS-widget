use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::install::install_dir;

const CONFIG_FILE: &str = "config.json";
const KEYRING_SERVICE: &str = "DuckDNS Widget";
const KEYRING_USER: &str = "duckdns-token";
const DUCKDNS_SUFFIX: &str = ".duckdns.org";

/// 평문으로 둬도 되는 값만 담습니다. 토큰은 자격 증명 관리자에 따로 둡니다.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetConfig {
    #[serde(default)]
    pub domains: Vec<String>,
    /// 확인 주기에 IP가 어긋나면 바로 갱신합니다. 홈페이지 버튼처럼 사용자가
    /// 직접 누르는 쪽이 기본이라 꺼 둔 채 시작합니다.
    #[serde(default)]
    pub auto_update: bool,
}

/// 설정 창에 보내는 값. 토큰 원문은 담지 않고 끝 4자리만 보입니다.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigView {
    pub domains: Vec<String>,
    pub auto_update: bool,
    pub has_token: bool,
    pub token_hint: Option<String>,
}

fn config_path() -> Result<PathBuf, String> {
    Ok(install_dir()?.join(CONFIG_FILE))
}

pub fn load_config() -> WidgetConfig {
    config_path()
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_config(config: &WidgetConfig) -> Result<(), String> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("설정 폴더 생성 실패: {e}"))?;
    }
    let text = serde_json::to_string_pretty(config).map_err(|e| format!("설정 직렬화 실패: {e}"))?;
    fs::write(&path, text).map_err(|e| format!("설정 저장 실패: {e}"))
}

/// 사용자가 무엇을 붙여 넣든 DuckDNS `domains` 파라미터가 받는 하위 이름만 남깁니다.
/// `https://myhome.duckdns.org/`, `MyHome.duckdns.org`, `myhome` 모두 `myhome`이 됩니다.
/// `www.myhome.duckdns.org`처럼 한 단계 더 붙은 이름은 DuckDNS가 `myhome`과 같은 레코드로
/// 다루므로 마지막 이름만 씁니다.
pub fn normalize_domain(raw: &str) -> Result<Option<String>, String> {
    let mut name = raw.trim().to_ascii_lowercase();
    if name.is_empty() {
        return Ok(None);
    }
    for scheme in ["https://", "http://"] {
        if let Some(rest) = name.strip_prefix(scheme) {
            name = rest.to_string();
        }
    }
    if let Some(slash) = name.find('/') {
        name.truncate(slash);
    }
    let name = name.trim_end_matches('.');
    if name == &DUCKDNS_SUFFIX[1..] {
        return Err(format!("하위 도메인 이름이 필요합니다: {}", raw.trim()));
    }
    let name = name.strip_suffix(DUCKDNS_SUFFIX).unwrap_or(name);
    let name = name.rsplit('.').next().unwrap_or(name);

    let valid = !name.is_empty()
        && name.len() <= 63
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid {
        return Err(format!("도메인 이름이 올바르지 않습니다: {}", raw.trim()));
    }
    Ok(Some(name.to_string()))
}

/// 쉼표·줄바꿈·공백 어느 것으로 나눠도 받습니다. 순서는 입력 그대로 두고 중복만 뺍니다.
pub fn parse_domains(raw: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(|c: char| c == ',' || c == ';' || c.is_whitespace()) {
        if let Some(name) = normalize_domain(part)? {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    Ok(out)
}

/// DuckDNS 토큰은 UUID 형식입니다. 형식이 바뀌어도 막히지 않게 길이와 문자만 봅니다.
pub fn validate_token(raw: &str) -> Result<String, String> {
    let token = raw.trim();
    let valid = (8..=64).contains(&token.len())
        && token.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if !valid {
        return Err("토큰 형식이 올바르지 않습니다. duckdns.org 상단의 token 값을 그대로 붙여 넣으세요.".into());
    }
    Ok(token.to_ascii_lowercase())
}

fn token_entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| format!("자격 증명 저장소 열기 실패: {e}"))
}

/// 반환된 토큰은 로그나 에러 문구에 넣지 않습니다.
pub fn read_token() -> Option<String> {
    token_entry()
        .ok()?
        .get_password()
        .ok()
        .filter(|t| !t.is_empty())
}

pub fn store_token(token: &str) -> Result<(), String> {
    token_entry()?
        .set_password(token)
        .map_err(|e| format!("토큰 저장 실패: {e}"))
}

pub fn delete_token() -> Result<(), String> {
    match token_entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("토큰 삭제 실패: {e}")),
    }
}

fn token_hint(token: &str) -> String {
    let tail: String = token.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("••••{tail}")
}

pub fn config_view() -> ConfigView {
    let config = load_config();
    let token = read_token();
    ConfigView {
        domains: config.domains,
        auto_update: config.auto_update,
        has_token: token.is_some(),
        token_hint: token.as_deref().map(token_hint),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_common_inputs() {
        for raw in [
            "myhome",
            "MyHome",
            " myhome.duckdns.org ",
            "https://myhome.duckdns.org/",
            "http://myhome.duckdns.org/path",
            "myhome.duckdns.org.",
            "www.myhome.duckdns.org",
        ] {
            assert_eq!(normalize_domain(raw).unwrap().as_deref(), Some("myhome"), "{raw}");
        }
    }

    #[test]
    fn normalize_rejects_garbage() {
        for raw in ["my_home", "-home", "home-", "duckdns.org", "한글"] {
            assert!(normalize_domain(raw).is_err(), "{raw}");
        }
        assert_eq!(normalize_domain("   ").unwrap(), None);
    }

    #[test]
    fn parse_splits_and_dedupes() {
        let parsed = parse_domains("myhome, nas.duckdns.org\nMYHOME;  cam ").unwrap();
        assert_eq!(parsed, vec!["myhome", "nas", "cam"]);
        assert!(parse_domains("").unwrap().is_empty());
    }

    #[test]
    fn token_validation() {
        assert_eq!(
            validate_token(" A7C4E5F0-1234-4BCD-9EF0-0123456789AB ").unwrap(),
            "a7c4e5f0-1234-4bcd-9ef0-0123456789ab"
        );
        assert!(validate_token("not a token").is_err());
        assert!(validate_token("abc").is_err());
    }

    /// 플랫폼 기능이 빠지면 keyring은 메모리 목(mock)으로 조용히 돌아갑니다.
    /// 새 Entry로 다시 읽혀야 실제 자격 증명 관리자에 들어간 것입니다.
    #[test]
    #[ignore]
    fn live_keyring_round_trip() {
        let service = "DuckDNS Widget (test)";
        let fake = "00000000-0000-0000-0000-00000000test";
        keyring::Entry::new(service, KEYRING_USER).unwrap().set_password(fake).unwrap();
        let read = keyring::Entry::new(service, KEYRING_USER).unwrap().get_password();
        keyring::Entry::new(service, KEYRING_USER).unwrap().delete_credential().unwrap();
        assert_eq!(read.unwrap(), fake);
    }

    #[test]
    fn hint_shows_only_tail() {
        assert_eq!(token_hint("a7c4e5f0-1234-4bcd-9ef0-0123456789ab"), "••••89ab");
    }
}
