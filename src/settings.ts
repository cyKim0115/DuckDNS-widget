import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { applyAlwaysOnTop, loadAlwaysOnTop } from "./preferences";
import {
  describeUpdate,
  domainStateLabel,
  stateLabel,
  type ConfigView,
  type DnsSnapshot,
} from "./types";

/** 위젯이 이미 갱신 중이면 결과 이벤트가 오지 않아서, 버튼을 영영 잠그지 않게 풀어 줍니다. */
const UPDATE_BUTTON_TIMEOUT_MS = 30_000;
/** 갱신 뒤 다시 확인하는 데 걸리는 시간보다 넉넉히 잡습니다. */
const FRESH_RECORD_MS = 60_000;
/** 제목 표시줄과 화면 위아래 여유. 이만큼 빼야 창 아래 끝이 작업 표시줄에 걸리지 않습니다. */
const SCREEN_MARGIN = 80;
/** tauri.conf.json의 minHeight와 같습니다. */
const MIN_HEIGHT = 480;

let config: ConfigView | null = null;
let autostartEnabled = false;
let isDevBuild = false;
let awaitingRecheck = false;
let updateButtonTimer: number | undefined;

function $(id: string): HTMLElement {
  const el = document.getElementById(id);
  if (!el) throw new Error(`missing #${id}`);
  return el;
}

/**
 * 에러 문자열에는 Rust가 옮겨 온 응답 문구가 섞일 수 있어서, 마크업을
 * 조립하지 않고 textContent로만 넣습니다.
 */
function showBootError(message: string) {
  const wrap = document.createElement("div");
  wrap.className = "settings-window";
  wrap.style.padding = "24px";

  const title = document.createElement("h1");
  title.textContent = "설정 로드 실패";

  const detail = document.createElement("p");
  detail.className = "settings-hint settings-hint--error";
  detail.textContent = message;

  const close = document.createElement("button");
  close.type = "button";
  close.className = "btn secondary wide";
  close.textContent = "닫기";
  close.addEventListener("click", () => {
    void closeWindow();
  });

  wrap.append(title, detail, close);
  document.body.replaceChildren(wrap);
}

function contentHeight(): number {
  const el = document.querySelector(".settings-window");
  return el ? Math.ceil(el.getBoundingClientRect().height) : 0;
}

/**
 * 화면에 들어가는 만큼만 내용 높이에 창을 맞추고 가운데로 옮깁니다. 내용이 더 길면
 * 스크롤로 넘기고 닫기 버튼은 아래에 붙어 있습니다. 고정 높이로 두면 FHD처럼 세로가
 * 짧은 화면에서 창 아래가 화면 밖으로 나가, 늘리지도 닫지도 못하게 됩니다.
 */
async function fitToScreen() {
  try {
    const win = getCurrentWindow();
    const inner = (await win.innerSize()).toLogical(await win.scaleFactor());
    const maxHeight = Math.max(MIN_HEIGHT, window.screen.availHeight - SCREEN_MARGIN);
    const target = Math.max(MIN_HEIGHT, Math.min(contentHeight(), maxHeight));
    if (Math.abs(target - inner.height) > 1) {
      await win.setSize(new LogicalSize(inner.width, target));
    }
    await win.center();
  } catch {
    /* 크기 조절 실패는 스크롤로 버팁니다 */
  }
}

/**
 * 위에 알림이 떠서 내용이 길어지면, 지금 자리에서 화면 아래 끝까지 남은 만큼만 늘립니다.
 * 사용자가 옮겨 둔 창을 움직이거나 줄이지는 않습니다. 모자라는 만큼은 스크롤입니다.
 */
async function growToContent() {
  try {
    const win = getCurrentWindow();
    const scale = await win.scaleFactor();
    const inner = (await win.innerSize()).toLogical(scale);
    const needed = contentHeight();
    if (needed <= inner.height + 1) return;

    const top = (await win.innerPosition()).toLogical(scale).y;
    const availTop = (window.screen as Screen & { availTop?: number }).availTop ?? 0;
    const room = Math.floor(availTop + window.screen.availHeight - top - 8);
    const target = Math.min(needed, room);
    if (target > inner.height + 1) {
      await win.setSize(new LogicalSize(inner.width, target));
    }
  } catch {
    /* 크기 조절 실패는 스크롤로 버팁니다 */
  }
}

function setActionStatus(message: string, tone: "info" | "ok" | "error" = "info") {
  const el = $("action-status");
  el.textContent = message;
  el.classList.remove("hidden", "settings-status--ok", "settings-status--error");
  if (tone === "ok") el.classList.add("settings-status--ok");
  else if (tone === "error") el.classList.add("settings-status--error");
  void growToContent();
}

function clearActionStatus() {
  $("action-status").classList.add("hidden");
  $("action-status").textContent = "";
}

function badgeClass(state: string): string {
  if (state === "OK") return "settings-badge--ok";
  if (state === "Mismatch" || state === "NotConfigured") return "settings-badge--muted";
  return "settings-badge--error";
}

function tokenInput(): HTMLInputElement {
  return $("token-input") as HTMLInputElement;
}

function domainsInput(): HTMLTextAreaElement {
  return $("domains-input") as HTMLTextAreaElement;
}

/** 저장 결과는 정규화된 이름(`myhome`)으로 돌아와서, 입력칸도 그 모양으로 다시 채웁니다. */
function renderConfig(view: ConfigView) {
  config = view;
  $("token-state").textContent = view.hasToken
    ? `저장됨 · ${view.tokenHint ?? ""}`
    : "저장된 토큰 없음";
  tokenInput().value = "";
  domainsInput().value = view.domains.join(", ");
  ($("auto-update-toggle") as HTMLInputElement).checked = view.autoUpdate;
  ($("btn-clear-token") as HTMLButtonElement).disabled = !view.hasToken;
}

/** 입력칸에 저장하지 않은 값이 있는지. 저장 전 값으로 갱신하면 결과가 헷갈립니다. */
function hasUnsavedInput(): boolean {
  if (tokenInput().value.trim() !== "") return true;
  const saved = (config?.domains ?? []).join(", ");
  return domainsInput().value.trim() !== saved;
}

function summaryItem(name: string, value: string, tone = ""): HTMLLIElement {
  const li = document.createElement("li");
  if (tone) li.className = tone;

  const label = document.createElement("span");
  label.className = "ip-summary-name";
  label.textContent = name;

  const detail = document.createElement("span");
  detail.className = "ip-summary-value";
  detail.textContent = value;

  li.append(label, detail);
  return li;
}

function renderStatus(snap: DnsSnapshot) {
  const badge = $("connection-badge");
  badge.textContent = stateLabel(snap.state);
  badge.className = `settings-badge ${badgeClass(snap.state)}`;

  const items = [summaryItem("내 IP", snap.publicIp ?? "확인 실패", snap.publicIp ? "" : "unknown")];
  for (const domain of snap.domains) {
    items.push(
      summaryItem(
        `${domain.name}.duckdns.org`,
        `${domain.resolvedIp ?? "—"} · ${domainStateLabel(domain.status)}`,
        domain.status,
      ),
    );
  }
  $("ip-summary").replaceChildren(...items);

  $("last-update").textContent = snap.lastUpdate
    ? describeUpdate(snap.lastUpdate)
    : "갱신 기록 없음";

  const updateBtn = $("btn-update") as HTMLButtonElement;
  if (updateButtonTimer === undefined) {
    updateBtn.disabled = !snap.hasToken || snap.domains.length === 0;
  }

  if (awaitingRecheck) {
    awaitingRecheck = false;
    if (snap.state === "FetchError") setActionStatus(snap.error ?? "확인하지 못했습니다.", "error");
    else setActionStatus(`다시 확인했습니다 · ${stateLabel(snap.state)}`, "ok");
  }
}

function renderUpdateResult(snap: DnsSnapshot) {
  window.clearTimeout(updateButtonTimer);
  updateButtonTimer = undefined;
  ($("btn-update") as HTMLButtonElement).disabled = !snap.hasToken || snap.domains.length === 0;

  // 토큰이 없으면 갱신 요청 자체가 나가지 않아 지난 기록이 그대로 옵니다.
  // 방금 만든 기록일 때만 이번 결과로 읽습니다.
  const rec = snap.lastUpdate;
  const fresh = !!rec && snap.checkedAtMs - rec.atMs < FRESH_RECORD_MS;
  if (!rec || !fresh || !rec.ok) {
    setActionStatus(snap.error ?? rec?.message ?? "갱신하지 못했습니다.", "error");
    return;
  }
  const result = rec.changed ? "IP를 바꿨습니다" : "이미 같은 IP라 바뀐 것이 없습니다";
  setActionStatus(rec.ip ? `갱신했습니다 — ${result} (${rec.ip})` : `갱신했습니다 — ${result}`, "ok");
}

function renderAlwaysOnTop() {
  ($("always-on-top-toggle") as HTMLInputElement).checked = loadAlwaysOnTop();
}

function syncAutostartUi() {
  const toggle = $("autostart-toggle") as HTMLInputElement;
  const hint = $("autostart-hint");

  if (isDevBuild) {
    toggle.checked = false;
    toggle.disabled = true;
    hint.textContent =
      "개발 모드에서는 시작프로그램을 바꿀 수 없습니다. 「시작.bat」으로 설치·실행한 뒤 다시 시도하세요.";
    hint.classList.remove("hidden");
    return;
  }

  toggle.disabled = false;
  toggle.checked = autostartEnabled;
  hint.classList.add("hidden");
}

async function refreshAutostart() {
  isDevBuild = await invoke<boolean>("is_dev_build");
  autostartEnabled = await invoke<boolean>("is_autostart_enabled");
  syncAutostartUi();
}

async function loadConfig() {
  renderConfig(await invoke<ConfigView>("get_config"));
}

/**
 * 확인·갱신은 위젯이 혼자 합니다. 설정 창이 직접 부르면 두 창의 요청이 겹치므로,
 * 필요할 때 위젯에 요청해 그 결과(status-updated)를 받아 그립니다.
 */
async function requestStatus() {
  await emit("status-requested");
}

async function closeWindow() {
  await invoke("close_settings_window");
}

async function refreshView() {
  clearActionStatus();
  renderAlwaysOnTop();
  // 위젯이 주기마다 보내는 상태를 숨긴 채로도 받아 그려 두어서, 내용 높이는 이미 맞습니다.
  // 값을 다시 읽기 전에 먼저 맞춰야 창이 뜬 뒤 크기가 튀는 시간이 짧습니다.
  await fitToScreen();
  await refreshAutostart();
  await loadConfig();
  await requestStatus();
}

async function onSave() {
  clearActionStatus();
  const btn = $("btn-save") as HTMLButtonElement;
  btn.disabled = true;
  try {
    const token = tokenInput().value.trim();
    const view = await invoke<ConfigView>("save_config", {
      domains: domainsInput().value,
      token: token === "" ? null : token,
    });
    renderConfig(view);
    await emit("config-changed");
    setActionStatus(
      view.domains.length > 0 && view.hasToken
        ? "저장했습니다. 위젯이 다시 확인합니다."
        : "저장했습니다. 토큰과 도메인이 모두 있어야 갱신할 수 있습니다.",
      "ok",
    );
  } catch (e) {
    setActionStatus(String(e), "error");
  } finally {
    btn.disabled = false;
  }
}

async function onClearToken() {
  if (!window.confirm("저장된 DuckDNS 토큰을 지울까요?")) return;
  clearActionStatus();
  try {
    const view = await invoke<ConfigView>("clear_token");
    // 도메인 입력칸에 저장 안 한 수정이 있을 수 있어 토큰 표시만 바꿉니다.
    const pendingDomains = domainsInput().value;
    renderConfig(view);
    domainsInput().value = pendingDomains;
    await emit("config-changed");
    setActionStatus("토큰을 지웠습니다.", "ok");
  } catch (e) {
    setActionStatus(String(e), "error");
  }
}

async function onUpdate() {
  if (hasUnsavedInput()) {
    setActionStatus("저장하지 않은 입력이 있습니다. 먼저 저장한 뒤 갱신하세요.", "error");
    return;
  }
  const btn = $("btn-update") as HTMLButtonElement;
  btn.disabled = true;
  window.clearTimeout(updateButtonTimer);
  updateButtonTimer = window.setTimeout(() => {
    updateButtonTimer = undefined;
    btn.disabled = false;
  }, UPDATE_BUTTON_TIMEOUT_MS);
  setActionStatus("DuckDNS에 현재 IP로 갱신을 요청하고 있습니다…");
  try {
    await emit("update-requested");
  } catch (e) {
    window.clearTimeout(updateButtonTimer);
    updateButtonTimer = undefined;
    btn.disabled = false;
    setActionStatus(String(e), "error");
  }
}

async function onRefresh() {
  clearActionStatus();
  awaitingRecheck = true;
  setActionStatus("내 IP와 등록 IP를 다시 확인하고 있습니다…");
  try {
    await emit("refresh-requested");
  } catch (e) {
    awaitingRecheck = false;
    setActionStatus(String(e), "error");
  }
}

async function boot() {
  try {
    renderAlwaysOnTop();
    await refreshAutostart();
    await loadConfig();

    $("btn-close").addEventListener("click", () => {
      void closeWindow();
    });
    $("btn-save").addEventListener("click", () => {
      void onSave();
    });
    $("btn-clear-token").addEventListener("click", () => {
      void onClearToken();
    });
    $("btn-update").addEventListener("click", () => {
      void onUpdate();
    });
    $("btn-refresh").addEventListener("click", () => {
      void onRefresh();
    });

    const autoUpdateToggle = $("auto-update-toggle") as HTMLInputElement;
    autoUpdateToggle.addEventListener("change", async () => {
      const enabled = autoUpdateToggle.checked;
      try {
        const view = await invoke<ConfigView>("set_auto_update", { enabled });
        config = view;
        autoUpdateToggle.checked = view.autoUpdate;
        await emit("config-changed");
        setActionStatus(
          enabled ? "IP가 바뀌면 자동으로 갱신합니다." : "자동 갱신을 껐습니다. 위젯 버튼으로 갱신하세요.",
          "ok",
        );
      } catch (e) {
        autoUpdateToggle.checked = config?.autoUpdate ?? false;
        setActionStatus(String(e), "error");
      }
    });

    const alwaysOnTopToggle = $("always-on-top-toggle") as HTMLInputElement;
    alwaysOnTopToggle.addEventListener("change", async () => {
      const enabled = alwaysOnTopToggle.checked;
      try {
        await applyAlwaysOnTop(enabled);
        setActionStatus(
          enabled ? "위젯을 항상 위에 표시합니다." : "위젯을 일반 창처럼 표시합니다.",
          "ok",
        );
      } catch (e) {
        alwaysOnTopToggle.checked = loadAlwaysOnTop();
        setActionStatus(String(e), "error");
      }
    });

    const autostartToggle = $("autostart-toggle") as HTMLInputElement;
    autostartToggle.addEventListener("change", async () => {
      if (isDevBuild) {
        syncAutostartUi();
        return;
      }
      autostartToggle.disabled = true;
      try {
        if (autostartToggle.checked) {
          await invoke("enable_autostart");
        } else {
          await invoke("disable_autostart");
        }
        autostartEnabled = await invoke<boolean>("is_autostart_enabled");
        syncAutostartUi();
        setActionStatus(
          autostartEnabled ? "시작프로그램을 켰습니다." : "시작프로그램을 껐습니다.",
          "ok",
        );
      } catch (e) {
        autostartToggle.checked = autostartEnabled;
        setActionStatus(String(e), "error");
      } finally {
        autostartToggle.disabled = isDevBuild;
      }
    });

    // 숨겨져 있을 때는 다음에 열릴 크기를 미리 맞춰 두고, 떠 있을 때는 도메인 줄이
    // 늘어난 만큼만 키웁니다.
    await listen<DnsSnapshot>("status-updated", async (event) => {
      renderStatus(event.payload);
      const visible = await getCurrentWindow()
        .isVisible()
        .catch(() => true);
      void (visible ? growToContent() : fitToScreen());
    });

    await listen<DnsSnapshot>("update-finished", (event) => {
      renderUpdateResult(event.payload);
    });

    // 창은 숨겨둔 채 재사용하므로, 다시 열릴 때마다 값을 새로 읽습니다.
    await listen("settings-open", () => {
      void refreshView();
    });

    window.addEventListener("keydown", (event) => {
      if (event.key === "Escape") void closeWindow();
    });

    await requestStatus();
  } catch (e) {
    showBootError(String(e));
  }
}

void boot();
