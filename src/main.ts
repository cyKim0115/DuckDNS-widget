import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { loadAlwaysOnTop } from "./preferences";
import { domainStateLabel, formatClock, type DnsSnapshot, type DomainStatus } from "./types";

const WINDOW_WIDTH = 340;

let lastSnapshot: DnsSnapshot | null = null;
let updating = false;

function $(id: string): HTMLElement {
  const el = document.getElementById(id);
  if (!el) throw new Error(`missing #${id}`);
  return el;
}

function hideContextMenu() {
  $("context-menu").classList.add("hidden");
  $("context-backdrop").classList.add("hidden");
}

/**
 * 메뉴에는 설정과 종료만 둡니다. 창이 도메인 수에 맞춰 150px 안팎까지 줄어들어서,
 * 항목이 늘어나면 메뉴가 창 밖으로 잘려 종료를 누를 수 없게 됩니다. 나머지
 * 항목은 별도 설정 창(settings.html)으로 옮겼습니다.
 */
function showContextMenu(x: number, y: number) {
  const backdrop = $("context-backdrop");
  const menu = $("context-menu");

  backdrop.classList.remove("hidden");
  menu.classList.remove("hidden");

  const menuRect = menu.getBoundingClientRect();
  const maxX = Math.max(8, window.innerWidth - menuRect.width - 8);
  const maxY = Math.max(8, window.innerHeight - menuRect.height - 8);

  menu.style.left = `${Math.min(x, maxX)}px`;
  menu.style.top = `${Math.min(y, maxY)}px`;
}

function buildDomainRow(domain: DomainStatus): HTMLElement {
  const row = document.createElement("section");
  row.className = `domain-row ${domain.status}`;
  row.title = `${domain.name}.duckdns.org`;
  row.setAttribute("data-tauri-drag-region", "");

  const dot = document.createElement("span");
  dot.className = "dot";
  dot.setAttribute("aria-hidden", "true");

  const label = document.createElement("span");
  label.className = "label";
  label.textContent = domain.name;
  label.setAttribute("data-tauri-drag-region", "");

  const caption = document.createElement("span");
  caption.className = "caption";
  caption.textContent = `${domain.resolvedIp ?? "—"} · ${domainStateLabel(domain.status)}`;
  caption.setAttribute("data-tauri-drag-region", "");

  row.append(dot, label, caption);
  return row;
}

function renderEmpty(message: string): HTMLElement {
  const empty = document.createElement("div");
  empty.className = "empty";
  empty.textContent = message;
  empty.setAttribute("data-tauri-drag-region", "");
  return empty;
}

/** 도메인 수가 설정마다 달라서, 그린 다음 실제 높이에 창을 맞춥니다. */
async function syncWindowHeight() {
  await new Promise((resolve) => requestAnimationFrame(resolve));
  const height = Math.ceil($("widget").getBoundingClientRect().height);
  if (height <= 0) return;
  try {
    await getCurrentWindow().setSize(new LogicalSize(WINDOW_WIDTH, height));
  } catch {
    /* 크기 조절 실패는 표시를 막지 않습니다 */
  }
}

/** 이번 응답이 방금 실패한 갱신의 결과인지. 지난 실패 기록만으로는 경고를 띄우지 않습니다. */
function updateJustFailed(snap: DnsSnapshot): boolean {
  const rec = snap.lastUpdate;
  return !!rec && !rec.ok && !!snap.error && rec.message === snap.error;
}

function canUpdate(snap: DnsSnapshot | null): boolean {
  return !!snap && snap.hasToken && snap.domains.length > 0;
}

function renderButton() {
  const btn = $("btn-update") as HTMLButtonElement;
  const snap = lastSnapshot;
  btn.disabled = updating || !canUpdate(snap);
  btn.classList.toggle("attention", snap?.state === "Mismatch");
  btn.textContent = updating ? "갱신 중…" : "현재 IP로 갱신";
}

function statusText(snap: DnsSnapshot): string {
  const checked = formatClock(snap.checkedAtMs);
  if (updateJustFailed(snap)) return `갱신 실패 · ${checked}`;
  if (snap.state === "NotConfigured") {
    return snap.domains.length === 0 ? "설정 필요" : "토큰 필요 · 우클릭 → 설정";
  }
  if (snap.state === "FetchError") return `IP 확인 실패 · ${checked}`;

  const parts = [`${checked} 확인`];
  if (snap.lastUpdate?.ok) parts.push(`${formatClock(snap.lastUpdate.atMs)} 갱신`);
  if (snap.autoUpdate) parts.push("자동");
  return parts.join(" · ");
}

function render(snap: DnsSnapshot) {
  lastSnapshot = snap;

  const widget = $("widget");
  widget.classList.toggle("error", snap.state === "FetchError" || updateJustFailed(snap));
  widget.classList.toggle("attention", snap.state === "Mismatch");

  $("public-ip").textContent = snap.publicIp ? `내 IP ${snap.publicIp}` : "";

  const domains = $("domains");
  domains.replaceChildren();
  if (snap.domains.length > 0) {
    domains.append(...snap.domains.map(buildDomainRow));
  } else if (snap.state === "FetchError") {
    domains.append(renderEmpty(snap.error ?? "상태를 불러오지 못했습니다"));
  } else {
    domains.append(renderEmpty("우클릭 → 설정에서 토큰과 도메인을 입력하세요"));
  }

  const status = $("status");
  status.textContent = statusText(snap);
  // 실패 원인은 길어서 줄에 다 넣지 않고 마우스를 올리면 보이게 둡니다.
  status.title = snap.error ?? "";

  renderButton();
  void syncWindowHeight();

  // 설정 창이 상태를 직접 조회하지 않고 이 값을 받아 그립니다.
  void emit("status-updated", snap).catch(() => {
    /* 설정 창이 없을 수도 있어 실패는 무시합니다 */
  });
}

function renderFailure(e: unknown) {
  render({
    state: "FetchError",
    publicIp: null,
    domains: [],
    lastUpdate: lastSnapshot?.lastUpdate ?? null,
    autoUpdate: lastSnapshot?.autoUpdate ?? false,
    hasToken: lastSnapshot?.hasToken ?? false,
    checkedAtMs: Date.now(),
    error: String(e),
  });
}

async function refresh() {
  // 갱신 중이면 그 결과가 곧 새 상태라 따로 확인하지 않습니다.
  if (updating) return;
  try {
    render(await invoke<DnsSnapshot>("get_status"));
  } catch (e) {
    renderFailure(e);
  }
}

/** 홈페이지의 update ip 버튼과 같은 일. 설정 창의 지금 갱신도 이리로 옵니다. */
async function runUpdate() {
  if (updating) return;
  updating = true;
  renderButton();
  $("status").textContent = "갱신 중…";
  try {
    const snap = await invoke<DnsSnapshot>("update_now");
    updating = false;
    render(snap);
    void emit("update-finished", snap);
  } catch (e) {
    updating = false;
    renderFailure(e);
  }
}

async function boot() {
  const backdrop = $("context-backdrop");
  const menuSettings = $("menu-settings") as HTMLButtonElement;
  const menuQuit = $("menu-quit") as HTMLButtonElement;

  // tauri.conf.json pins the window to always-on-top, so a user who turned it
  // off gets it restored as soon as the webview boots.
  try {
    await invoke("set_always_on_top", { enabled: loadAlwaysOnTop() });
  } catch {
    /* browser preview */
  }

  window.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    showContextMenu(event.clientX, event.clientY);
  });

  backdrop.addEventListener("pointerdown", (event) => {
    if (event.target === backdrop) hideContextMenu();
  });

  $("context-menu").addEventListener("pointerdown", (event) => {
    event.stopPropagation();
  });

  window.addEventListener("blur", () => {
    hideContextMenu();
  });

  window.addEventListener("resize", () => {
    hideContextMenu();
  });

  window.addEventListener("click", () => {
    hideContextMenu();
  });

  window.addEventListener("keydown", (event) => {
    if (event.key === "Escape") hideContextMenu();
  });

  $("btn-update").addEventListener("click", (event) => {
    event.stopPropagation();
    void runUpdate();
  });

  menuSettings.addEventListener("click", async (event) => {
    event.stopPropagation();
    hideContextMenu();
    try {
      await invoke("open_settings_window");
    } catch (e) {
      window.alert(String(e));
    }
  });

  menuQuit.addEventListener("click", async (event) => {
    event.stopPropagation();
    hideContextMenu();
    await invoke("quit_app");
  });

  // 설정 창이 열릴 때 표시할 상태를 요구합니다. 아직 한 번도 못 받아왔다면
  // 새로 조회해서, 설정 창이 빈 값으로 남지 않게 합니다.
  await listen("status-requested", () => {
    if (lastSnapshot) {
      void emit("status-updated", lastSnapshot);
    } else {
      void refresh();
    }
  });

  // 확인과 갱신은 위젯이 수행합니다. 두 창이 각자 부르면 요청이 겹칩니다.
  await listen("refresh-requested", () => {
    void refresh();
  });
  await listen("update-requested", () => {
    void runUpdate();
  });
  // 토큰·도메인·자동 갱신을 바꾸면 바로 다시 확인해 위젯에 반영합니다.
  await listen("config-changed", () => {
    void refresh();
  });

  await refresh();
  let interval = 300_000;
  try {
    interval = await invoke<number>("get_poll_interval_ms");
  } catch {
    /* keep default */
  }
  window.setInterval(() => {
    void refresh();
  }, interval);
}

void boot();
