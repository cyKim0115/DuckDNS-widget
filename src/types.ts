export type DomainState = "match" | "mismatch" | "pending" | "unknown";

export type DomainStatus = {
  name: string;
  resolvedIp: string | null;
  status: DomainState;
};

export type UpdateRecord = {
  atMs: number;
  ok: boolean;
  ip: string | null;
  changed: boolean;
  auto: boolean;
  domains: string[];
  message: string | null;
};

export type DnsSnapshot = {
  state: string;
  publicIp: string | null;
  domains: DomainStatus[];
  lastUpdate: UpdateRecord | null;
  autoUpdate: boolean;
  hasToken: boolean;
  checkedAtMs: number;
  error: string | null;
};

export type ConfigView = {
  domains: string[];
  autoUpdate: boolean;
  hasToken: boolean;
  tokenHint: string | null;
};

export function stateLabel(state: string): string {
  switch (state) {
    case "OK":
      return "일치";
    case "Mismatch":
      return "변경 필요";
    case "NotConfigured":
      return "설정 필요";
    default:
      return "확인 실패";
  }
}

export function domainStateLabel(status: DomainState): string {
  switch (status) {
    case "match":
      return "일치";
    case "mismatch":
      return "변경 필요";
    case "pending":
      return "전파 중";
    default:
      return "알 수 없음";
  }
}

/** 오늘이면 시:분만, 다른 날이면 월/일을 앞에 붙입니다. */
export function formatClock(ms: number): string {
  const date = new Date(ms);
  const hhmm = date.toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
  const today = new Date();
  if (date.toDateString() === today.toDateString()) return hhmm;
  return `${date.getMonth() + 1}/${date.getDate()} ${hhmm}`;
}

export function describeUpdate(record: UpdateRecord): string {
  const when = formatClock(record.atMs);
  const how = record.auto ? "자동 갱신" : "갱신";
  if (!record.ok) return `${when} ${how} 실패 — ${record.message ?? "원인 불명"}`;
  const result = record.changed ? "IP 변경됨" : "변경 없음";
  return record.ip ? `${when} ${how} · ${result} (${record.ip})` : `${when} ${how} · ${result}`;
}
