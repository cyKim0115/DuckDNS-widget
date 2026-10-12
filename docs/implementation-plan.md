# DuckDNS Widget — 구현 계획

작성일: 2026-10-12

## 목표

duckdns.org 홈페이지의 **update ip** 버튼(현재 접속 IP로 도메인 갱신)을 바탕화면 위젯 버튼 하나로 옮긴다.
위젯은 내 공인 IP와 DuckDNS에 등록된 IP를 주기적으로 비교해, 어긋나면 바로 보이게 한다.

## 기존 위젯에서 가져올 구조

기준은 `claude-usage-widget`(가장 최근·가장 작은 구성)이다. `gpt-usage-widget`의 async 명령, `git-multi-check-widget`의
`%LOCALAPPDATA%` 설정 파일 패턴을 함께 쓴다.

| 항목 | 따라갈 방식 |
|------|-------------|
| 스택 | Tauri v2 + Vite + TypeScript(바닐라), Rust 백엔드 `ureq` |
| 창 | `main`(투명·테두리 없음·항상 위·작업 표시줄 숨김, 폭 340, 높이는 내용에 맞춤) + `settings`(숨겨 둔 일반 창 show/hide) |
| 플러그인 | `single-instance`(재실행 시 기존 창 앞으로), `window-state`(위치만 저장) |
| 우클릭 메뉴 | **설정 · 종료** 두 항목만. 나머지는 설정 창 |
| 창 간 통신 | 조회·갱신은 위젯만 한다. 설정 창은 이벤트로 요청하고 결과 이벤트를 받아 그린다 |
| 설치·실행 | `시작.bat` → `scripts/launch-user.ps1`(소스가 더 새로우면 릴리스 재빌드) → `%LOCALAPPDATA%\DuckDnsWidget\duckdns-widget.exe` 설치·바탕화면 바로가기 |
| 자동 실행 | `auto-launch` + debug exe 등록 정리 + 개발 모드에서 토글 비활성 |
| 개발용 가드 | debug exe를 Vite 없이 켜면 안내 창 후 종료 |
| 항상 위 | `localStorage` 공유 + `set_always_on_top` 명령 |
| 아이콘 | `scripts/make_icon.py`로 `app-icon.png`·BMP ICO 생성 |
| 스크립트 | `start:app`, `rebuild:app`, `build:app`, `make:icon` |

## DuckDNS 연동

```
공인 IP     GET https://api.ipify.org            (실패 시 https://1.1.1.1/cdn-cgi/trace)
등록 IP     GET https://cloudflare-dns.com/dns-query?name=<sub>.duckdns.org&type=A  (DoH JSON, 실패 시 dns.google → 시스템 리졸버)
갱신        GET https://www.duckdns.org/update?domains=a,b&token=<token>&verbose=true
            → OK / <IPv4> / <IPv6> / UPDATED|NOCHANGE,  실패는 KO
```

- `ip` 파라미터는 비운다. 홈페이지 버튼과 같이 요청을 보낸 PC의 IPv4를 DuckDNS가 감지해 쓴다.
- 등록 IP는 OS DNS 캐시를 피하려고 DoH로 읽는다. 그래도 TTL(60초) 동안 옛 값이 보일 수 있어,
  갱신 성공 후 3분 동안은 "전파 중"으로 표시하고 불일치로 치지 않는다.
- 5분마다 확인한다. DuckDNS 권장 간격과 같다.
- IPv6는 범위 밖이다(감지가 IPv4만 됨).

## 저장

| 값 | 위치 |
|----|------|
| 토큰 | Windows 자격 증명 관리자(`keyring` 크레이트). 평문 파일에 쓰지 않는다 |
| 도메인 목록·자동 갱신 | `%LOCALAPPDATA%\DuckDnsWidget\config.json` |
| 마지막 갱신 기록 | `%LOCALAPPDATA%\DuckDnsWidget\last-update.json` |
| 항상 위 | `localStorage` (기존 위젯과 동일) |

토큰은 화면에 끝 4자리만 보이고, 로그·에러 문자열·커밋에 남기지 않는다. 네트워크 에러 문구에서 URL·토큰을 지운다.

## 화면

### 위젯 (340px)

```
DUCKDNS                         내 IP 203.0.113.20
● myhome                        203.0.113.20 · 일치
● nas                           192.0.2.4 · 변경 필요
[        현재 IP로 갱신        ]
10:32 확인 · 10:30 갱신됨
```

- 점 색: 일치 초록 · 변경 필요 주황 · 전파 중 파랑 · 알 수 없음 회색
- 갱신 버튼은 불일치가 있을 때 강조색, 평소엔 보조색. 누르면 "갱신 중…"으로 잠긴다
- 토큰·도메인이 없으면 "우클릭 → 설정에서 토큰과 도메인을 입력하세요"

### 설정 창

1. **DuckDNS** — 토큰(비밀번호 입력, 저장 시 끝 4자리 표시) · 도메인(쉼표/줄바꿈, `.duckdns.org` 붙여도 됨) · 저장 / 토큰 지우기
2. **갱신** — 내 IP·도메인별 등록 IP, 마지막 갱신 결과(UPDATED/NOCHANGE/KO), **지금 갱신** · **다시 확인**, **IP가 바뀌면 자동 갱신** 토글(기본 꺼짐)
3. **위젯** — 항상 다른 창 위에 표시
4. **앱** — Windows 시작 시 자동 실행

## 상태 값

| state | 뜻 |
|-------|----|
| `NotConfigured` | 토큰이나 도메인이 없음 |
| `OK` | 모든 도메인이 공인 IP와 일치(전파 중 포함) |
| `Mismatch` | 하나 이상 불일치 |
| `FetchError` | 공인 IP를 못 읽음(오프라인 등) |

도메인 상태: `match` · `mismatch` · `pending` · `unknown`

## Rust 명령

`get_status`, `update_now`, `get_config`, `save_config(domains, token?)`, `set_auto_update(enabled)`, `clear_token`,
`get_poll_interval_ms`, `quit_app`, `open_settings_window`, `close_settings_window`, `set_always_on_top`,
`is_dev_build`, `enable_autostart`, `disable_autostart`, `is_autostart_enabled`, `install_release_copy`

네트워크 명령은 `async` + `spawn_blocking`으로 UI 스레드를 막지 않는다. 갱신은 Mutex로 직렬화한다.

## 파일 구성

```
index.html / settings.html / 시작.bat / app-icon.png
src/        main.ts settings.ts preferences.ts types.ts styles.css
src-tauri/  src/{main,lib,install,config,duckdns}.rs  tauri.conf.json  capabilities/default.json  icons/
scripts/    launch-user.ps1 rebuild-restart.ps1 make_icon.py
docs/       implementation-plan.md images/
```

## 진행 순서

1. 스캐폴드: package·vite·tsconfig·tauri.conf·Cargo·아이콘·스크립트
2. 백엔드: `config.rs`(설정·토큰), `duckdns.rs`(공인 IP·DoH·갱신·상태 판정), `lib.rs`·`install.rs`
3. 프런트: 위젯(`main.ts`), 설정 창(`settings.ts`), 스타일
4. 검증: `cargo test`(파싱·정규화·판정 단위 테스트), `tsc`, 릴리스 빌드·설치·실행, 실제 공인 IP/DoH 조회 확인, 잘못된 토큰으로 KO 처리 확인
5. README·스크린샷, 커밋

## 검증 한계

실제 DuckDNS 토큰은 사용자만 입력한다. 에이전트는 가짜 토큰으로 KO 경로까지만 확인하고, 실제 갱신(UPDATED)은 사용자가 설정 창에서 토큰을 넣고 확인한다.
