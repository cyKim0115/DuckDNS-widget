use serde::{Deserialize, Serialize};
use std::fs;
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use crate::config::{load_config, read_token};
use crate::install::install_dir;

const UPDATE_URL: &str = "https://www.duckdns.org/update";
const LAST_UPDATE_FILE: &str = "last-update.json";
const USER_AGENT: &str = "duckdns-widget/0.1";

/// 갱신 직후엔 DNS가 TTL(60초) 동안 옛 값을 돌려줄 수 있습니다. 이 시간 안에서
/// 방금 갱신한 IP와 공인 IP가 같으면 불일치가 아니라 전파 중으로 봅니다.
const PROPAGATION_GRACE_MS: i64 = 180_000;
/// 자동 갱신이 직전 갱신과 겹치지 않게 둡니다.
const AUTO_UPDATE_COOLDOWN_MS: i64 = 60_000;
/// 토큰이 틀려 KO가 나면 확인 주기마다 같은 요청을 반복하지 않게 쉬어 갑니다.
const AUTO_UPDATE_FAILURE_BACKOFF_MS: i64 = 30 * 60_000;

/// 확인 주기의 자동 갱신과 버튼 갱신이 동시에 나가지 않게 묶습니다.
static UPDATE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRecord {
    pub at_ms: i64,
    pub ok: bool,
    /// DuckDNS가 응답한 등록 IP. 실패면 비어 있습니다.
    pub ip: Option<String>,
    /// `UPDATED`면 true, `NOCHANGE`면 false.
    pub changed: bool,
    pub auto: bool,
    #[serde(default)]
    pub domains: Vec<String>,
    pub message: Option<String>,
}

/// 위젯 한 줄에 해당합니다.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainStatus {
    pub name: String,
    pub resolved_ip: Option<String>,
    /// match · mismatch · pending · unknown
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DnsSnapshot {
    /// OK · Mismatch · NotConfigured · FetchError
    pub state: String,
    pub public_ip: Option<String>,
    pub domains: Vec<DomainStatus>,
    pub last_update: Option<UpdateRecord>,
    pub auto_update: bool,
    pub has_token: bool,
    pub checked_at_ms: i64,
    pub error: Option<String>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .user_agent(USER_AGENT)
        .build()
}

/// ureq의 에러 문구에는 요청 URL이 통째로 들어갑니다. 갱신 URL에는 토큰이 있어서
/// URL은 버리고 무엇이 실패했는지만 남깁니다.
fn describe_error(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => match t.kind() {
            ureq::ErrorKind::Dns => "DNS 조회 실패".into(),
            ureq::ErrorKind::ConnectionFailed => "연결 실패".into(),
            ureq::ErrorKind::Io => "네트워크 오류(시간 초과 등)".into(),
            other => other.to_string(),
        },
    }
}

fn get_text(request: ureq::Request) -> Result<String, String> {
    request
        .call()
        .map_err(describe_error)?
        .into_string()
        .map_err(|_| "응답을 읽지 못했습니다".to_string())
}

// --- 공인 IP ---

fn parse_plain_ip(body: &str) -> Option<Ipv4Addr> {
    body.trim().parse().ok()
}

/// Cloudflare trace는 `key=value` 줄로 옵니다. 그중 `ip=` 줄만 봅니다.
fn parse_trace_ip(body: &str) -> Option<Ipv4Addr> {
    body.lines()
        .find_map(|line| line.strip_prefix("ip="))
        .and_then(|value| value.trim().parse().ok())
}

/// DuckDNS가 `ip`를 비운 요청에서 감지하는 것도 IPv4라, 비교 기준도 IPv4로 맞춥니다.
/// 두 번째 공급자는 IPv4 주소로 직접 접속해서 IPv6 경로로 새지 않습니다.
pub fn fetch_public_ip() -> Result<Ipv4Addr, String> {
    let providers: [(&str, fn(&str) -> Option<Ipv4Addr>); 2] = [
        ("https://api.ipify.org", parse_plain_ip),
        ("https://1.1.1.1/cdn-cgi/trace", parse_trace_ip),
    ];
    let agent = agent();
    let mut last_error = String::from("공인 IP를 확인하지 못했습니다");
    for (url, parse) in providers {
        match get_text(agent.get(url)) {
            Ok(body) => match parse(&body) {
                Some(ip) => return Ok(ip),
                None => last_error = "공인 IP 응답 형식이 낯섭니다".into(),
            },
            Err(e) => last_error = format!("공인 IP 확인 실패: {e}"),
        }
    }
    Err(last_error)
}

// --- 등록 IP (DNS) ---

#[derive(Debug, Deserialize)]
struct DohResponse {
    #[serde(rename = "Status")]
    status: i32,
    #[serde(rename = "Answer", default)]
    answer: Vec<DohAnswer>,
}

#[derive(Debug, Deserialize)]
struct DohAnswer {
    #[serde(rename = "type")]
    kind: u16,
    data: String,
}

/// DoH JSON 응답에서 A 레코드를 꺼냅니다. 레코드가 없거나 NXDOMAIN이면 `None`입니다.
fn parse_doh(body: &str) -> Result<Option<Ipv4Addr>, String> {
    let parsed: DohResponse =
        serde_json::from_str(body).map_err(|_| "DNS 응답 형식이 낯섭니다".to_string())?;
    match parsed.status {
        0 => Ok(parsed
            .answer
            .iter()
            .filter(|a| a.kind == 1)
            .find_map(|a| a.data.trim().parse().ok())),
        3 => Ok(None),
        code => Err(format!("DNS 오류 코드 {code}")),
    }
}

/// OS DNS 캐시나 공유기 캐시를 거치지 않으려고 DoH로 먼저 묻습니다. 둘 다 막혀 있으면
/// 시스템 리졸버로 물러납니다.
pub fn resolve_domain(name: &str) -> Result<Option<Ipv4Addr>, String> {
    let fqdn = format!("{name}.duckdns.org");
    let agent = agent();
    let mut last_error = String::from("DNS 조회 실패");
    for url in ["https://cloudflare-dns.com/dns-query", "https://dns.google/resolve"] {
        let request = agent
            .get(url)
            .query("name", &fqdn)
            .query("type", "A")
            .set("accept", "application/dns-json");
        match get_text(request).and_then(|body| parse_doh(&body)) {
            Ok(ip) => return Ok(ip),
            Err(e) => last_error = e,
        }
    }

    match (fqdn.as_str(), 0).to_socket_addrs() {
        Ok(addrs) => Ok(addrs.into_iter().find_map(|addr| match addr {
            SocketAddr::V4(v4) => Some(*v4.ip()),
            SocketAddr::V6(_) => None,
        })),
        Err(_) => Err(last_error),
    }
}

// --- 갱신 ---

/// `verbose=true` 응답: `OK` / IPv4 / IPv6 / `UPDATED`|`NOCHANGE`. 실패는 `KO` 한 줄입니다.
fn parse_update_response(body: &str) -> Result<(Option<String>, bool), String> {
    let lines: Vec<&str> = body.lines().map(str::trim).collect();
    match lines.first().copied() {
        Some("OK") => {
            let ip = lines.get(1).filter(|v| !v.is_empty()).map(|v| v.to_string());
            let changed = lines.iter().any(|l| *l == "UPDATED");
            Ok((ip, changed))
        }
        Some("KO") => Err("DuckDNS가 갱신을 거부했습니다(KO). 토큰과 도메인을 확인하세요.".into()),
        _ => Err("DuckDNS 응답 형식이 낯섭니다".into()),
    }
}

/// `ip`는 비워 둡니다. 홈페이지의 update ip 버튼처럼, 요청을 보낸 PC의 IPv4를
/// DuckDNS가 직접 감지해 씁니다.
fn call_update(domains: &[String], token: &str) -> Result<(Option<String>, bool), String> {
    let request = agent()
        .get(UPDATE_URL)
        .query("domains", &domains.join(","))
        .query("token", token)
        .query("verbose", "true");
    get_text(request)
        .and_then(|body| parse_update_response(&body))
        // describe_error가 URL을 버리지만, 혹시 섞여 들어와도 토큰은 남기지 않습니다.
        .map_err(|e| e.replace(token, "•••"))
}

fn last_update_path() -> Option<PathBuf> {
    install_dir().ok().map(|dir| dir.join(LAST_UPDATE_FILE))
}

pub fn load_last_update() -> Option<UpdateRecord> {
    let text = fs::read_to_string(last_update_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_last_update(record: &UpdateRecord) {
    let Some(path) = last_update_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(record) {
        let _ = fs::write(path, text);
    }
}

fn perform_update(domains: &[String], token: &str, auto: bool) -> UpdateRecord {
    let _guard = UPDATE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let record = match call_update(domains, token) {
        Ok((ip, changed)) => UpdateRecord {
            at_ms: now_ms(),
            ok: true,
            ip,
            changed,
            auto,
            domains: domains.to_vec(),
            message: None,
        },
        Err(message) => UpdateRecord {
            at_ms: now_ms(),
            ok: false,
            ip: None,
            changed: false,
            auto,
            domains: domains.to_vec(),
            message: Some(message),
        },
    };
    save_last_update(&record);
    record
}

// --- 판정 ---

fn judge(
    name: &str,
    public_ip: Option<Ipv4Addr>,
    resolved: &Result<Option<Ipv4Addr>, String>,
    last: Option<&UpdateRecord>,
    now: i64,
) -> &'static str {
    let Some(public) = public_ip else {
        return "unknown";
    };
    let resolved = match resolved {
        Ok(ip) => *ip,
        // DNS를 못 읽은 건 불일치가 아닙니다. 모르는 채로 둡니다.
        Err(_) => return "unknown",
    };
    if resolved == Some(public) {
        return "match";
    }
    let just_updated = last.is_some_and(|rec| {
        rec.ok
            && now - rec.at_ms < PROPAGATION_GRACE_MS
            && rec.domains.iter().any(|d| d == name)
            && rec.ip.as_deref() == Some(public.to_string().as_str())
    });
    if just_updated {
        "pending"
    } else {
        "mismatch"
    }
}

struct Observation {
    public_ip: Result<Ipv4Addr, String>,
    resolved: Vec<(String, Result<Option<Ipv4Addr>, String>)>,
}

fn observe(domains: &[String]) -> Observation {
    Observation {
        public_ip: fetch_public_ip(),
        resolved: domains
            .iter()
            .map(|name| (name.clone(), resolve_domain(name)))
            .collect(),
    }
}

fn build_snapshot(
    obs: &Observation,
    last: Option<UpdateRecord>,
    auto_update: bool,
    has_token: bool,
    now: i64,
) -> DnsSnapshot {
    let public_ip = obs.public_ip.as_ref().ok().copied();
    let domains: Vec<DomainStatus> = obs
        .resolved
        .iter()
        .map(|(name, resolved)| DomainStatus {
            name: name.clone(),
            resolved_ip: resolved.as_ref().ok().copied().flatten().map(|ip| ip.to_string()),
            status: judge(name, public_ip, resolved, last.as_ref(), now).into(),
        })
        .collect();

    let state = if domains.is_empty() || !has_token {
        "NotConfigured"
    } else if public_ip.is_none() {
        "FetchError"
    } else if domains.iter().any(|d| d.status == "mismatch") {
        "Mismatch"
    } else {
        "OK"
    };

    DnsSnapshot {
        state: state.into(),
        public_ip: public_ip.map(|ip| ip.to_string()),
        domains,
        last_update: last,
        auto_update,
        has_token,
        checked_at_ms: now,
        error: obs.public_ip.as_ref().err().cloned(),
    }
}

fn auto_update_allowed(last: Option<&UpdateRecord>, now: i64) -> bool {
    match last {
        None => true,
        Some(rec) if now - rec.at_ms < AUTO_UPDATE_COOLDOWN_MS => false,
        Some(rec) if rec.auto && !rec.ok && now - rec.at_ms < AUTO_UPDATE_FAILURE_BACKOFF_MS => false,
        Some(_) => true,
    }
}

/// 위젯의 확인 주기. 자동 갱신이 켜져 있고 IP가 어긋나 있으면 그 자리에서 갱신합니다.
pub fn check() -> DnsSnapshot {
    let config = load_config();
    let token = read_token();
    let obs = observe(&config.domains);
    let last = load_last_update();
    let now = now_ms();
    let snapshot = build_snapshot(&obs, last.clone(), config.auto_update, token.is_some(), now);

    let Some(token) = token else {
        return snapshot;
    };
    if !config.auto_update || snapshot.state != "Mismatch" || !auto_update_allowed(last.as_ref(), now) {
        return snapshot;
    }

    let record = perform_update(&config.domains, &token, true);
    let error = record.message.clone();
    let mut snapshot = build_snapshot(&obs, Some(record), config.auto_update, true, now_ms());
    snapshot.error = error.or(snapshot.error);
    snapshot
}

/// 홈페이지의 update ip 버튼과 같은 일을 합니다. 갱신한 다음 다시 확인해 돌려줍니다.
pub fn update_now() -> DnsSnapshot {
    let config = load_config();
    let token = read_token();

    let Some(token) = token.filter(|_| !config.domains.is_empty()) else {
        let mut snapshot = check();
        snapshot.error = Some("설정에서 토큰과 도메인을 먼저 입력하세요.".into());
        return snapshot;
    };

    let record = perform_update(&config.domains, &token, false);
    let error = record.message.clone();
    let obs = observe(&config.domains);
    let mut snapshot = build_snapshot(&obs, Some(record), config.auto_update, true, now_ms());
    snapshot.error = error.or(snapshot.error);
    snapshot
}

/// 백그라운드 작업이 패닉으로 끝났을 때 위젯이 빈손으로 남지 않게 합니다.
pub fn internal_error(message: String) -> DnsSnapshot {
    DnsSnapshot {
        state: "FetchError".into(),
        public_ip: None,
        domains: Vec::new(),
        last_update: load_last_update(),
        auto_update: false,
        has_token: false,
        checked_at_ms: now_ms(),
        error: Some(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    fn record(at_ms: i64, ok: bool, ip: Option<&str>, auto: bool) -> UpdateRecord {
        UpdateRecord {
            at_ms,
            ok,
            ip: ip.map(String::from),
            changed: true,
            auto,
            domains: vec!["home".into()],
            message: None,
        }
    }

    #[test]
    fn update_response_verbose_ok() {
        let (got_ip, changed) = parse_update_response("OK\n211.234.10.20\n\nUPDATED").unwrap();
        assert_eq!(got_ip.as_deref(), Some("211.234.10.20"));
        assert!(changed);

        let (_, changed) = parse_update_response("OK\r\n211.234.10.20\r\n\r\nNOCHANGE\r\n").unwrap();
        assert!(!changed);

        let (got_ip, _) = parse_update_response("OK").unwrap();
        assert_eq!(got_ip, None);
    }

    #[test]
    fn update_response_ko_and_garbage() {
        assert!(parse_update_response("KO").unwrap_err().contains("KO"));
        assert!(parse_update_response("<html>").is_err());
        assert!(parse_update_response("").is_err());
    }

    #[test]
    fn trace_and_plain_ip() {
        let trace = "fl=123\nh=1.1.1.1\nip=211.234.10.20\nts=1\n";
        assert_eq!(parse_trace_ip(trace), Some(ip("211.234.10.20")));
        assert_eq!(parse_trace_ip("ip=2001:db8::1\n"), None);
        assert_eq!(parse_plain_ip(" 211.234.10.20\n"), Some(ip("211.234.10.20")));
    }

    #[test]
    fn doh_parsing() {
        let ok = r#"{"Status":0,"Answer":[{"name":"x.duckdns.org","type":5,"TTL":60,"data":"y."},{"name":"x.duckdns.org","type":1,"TTL":60,"data":"1.2.3.4"}]}"#;
        assert_eq!(parse_doh(ok).unwrap(), Some(ip("1.2.3.4")));
        assert_eq!(parse_doh(r#"{"Status":0}"#).unwrap(), None);
        assert_eq!(parse_doh(r#"{"Status":3}"#).unwrap(), None);
        assert!(parse_doh(r#"{"Status":2}"#).is_err());
        assert!(parse_doh("nope").is_err());
    }

    #[test]
    fn judge_states() {
        let public = Some(ip("5.6.7.8"));
        let now = 1_000_000;
        assert_eq!(judge("home", public, &Ok(Some(ip("5.6.7.8"))), None, now), "match");
        assert_eq!(judge("home", public, &Ok(Some(ip("1.2.3.4"))), None, now), "mismatch");
        assert_eq!(judge("home", public, &Ok(None), None, now), "mismatch");
        assert_eq!(judge("home", public, &Err("x".into()), None, now), "unknown");
        assert_eq!(judge("home", None, &Ok(Some(ip("1.2.3.4"))), None, now), "unknown");

        let fresh = record(now - 10_000, true, Some("5.6.7.8"), false);
        assert_eq!(judge("home", public, &Ok(Some(ip("1.2.3.4"))), Some(&fresh), now), "pending");
        // 다른 도메인을 갱신한 기록은 전파 중으로 치지 않습니다.
        assert_eq!(judge("nas", public, &Ok(Some(ip("1.2.3.4"))), Some(&fresh), now), "mismatch");

        let stale = record(now - PROPAGATION_GRACE_MS - 1, true, Some("5.6.7.8"), false);
        assert_eq!(judge("home", public, &Ok(Some(ip("1.2.3.4"))), Some(&stale), now), "mismatch");

        let failed = record(now - 10_000, false, None, false);
        assert_eq!(judge("home", public, &Ok(Some(ip("1.2.3.4"))), Some(&failed), now), "mismatch");
    }

    #[test]
    fn auto_update_throttle() {
        let now = 10_000_000;
        assert!(auto_update_allowed(None, now));
        assert!(!auto_update_allowed(Some(&record(now - 5_000, true, None, false)), now));
        assert!(auto_update_allowed(Some(&record(now - 120_000, true, None, true)), now));
        assert!(!auto_update_allowed(Some(&record(now - 120_000, false, None, true)), now));
        assert!(auto_update_allowed(
            Some(&record(now - AUTO_UPDATE_FAILURE_BACKOFF_MS - 1, false, None, true)),
            now
        ));
        // 수동 갱신 실패는 자동 갱신을 막지 않습니다.
        assert!(auto_update_allowed(Some(&record(now - 120_000, false, None, false)), now));
    }

    #[test]
    fn snapshot_states() {
        let now = 1_000_000;
        let obs = Observation {
            public_ip: Ok(ip("5.6.7.8")),
            resolved: vec![
                ("home".into(), Ok(Some(ip("5.6.7.8")))),
                ("nas".into(), Ok(Some(ip("1.2.3.4")))),
            ],
        };
        assert_eq!(build_snapshot(&obs, None, false, true, now).state, "Mismatch");
        assert_eq!(build_snapshot(&obs, None, false, false, now).state, "NotConfigured");

        let offline = Observation {
            public_ip: Err("공인 IP 확인 실패".into()),
            resolved: vec![("home".into(), Ok(Some(ip("5.6.7.8"))))],
        };
        let snap = build_snapshot(&offline, None, false, true, now);
        assert_eq!(snap.state, "FetchError");
        assert_eq!(snap.domains[0].status, "unknown");
        assert!(snap.error.is_some());

        let empty = Observation { public_ip: Ok(ip("5.6.7.8")), resolved: vec![] };
        assert_eq!(build_snapshot(&empty, None, false, true, now).state, "NotConfigured");
    }

    /// 실제 네트워크를 씁니다. `cargo test -- --ignored live`로만 돌립니다.
    #[test]
    #[ignore]
    fn live_lookups_and_rejected_token() {
        let public = fetch_public_ip().expect("public ip");
        assert!(!public.is_private() && !public.is_loopback());

        let resolved = resolve_domain("www").expect("doh");
        assert!(resolved.is_some(), "www.duckdns.org should resolve");

        let fake_token = "00000000-0000-0000-0000-000000000000";
        let err = call_update(&["duckdns-widget-nonexistent-test".into()], fake_token).unwrap_err();
        assert!(err.contains("KO"), "{err}");
        assert!(!err.contains(fake_token));
    }
}
