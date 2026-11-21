import { useCallback, useEffect, useState } from "react";

export const REFRESH_MS = 5000;

// ?mock=1 时用内置样例数据，不发请求，方便没有服务端时检查界面。
export const MOCK = (() => {
  try {
    return new URLSearchParams(window.location.search).get("mock") === "1";
  } catch {
    return false;
  }
})();

const NOW = 1795249805123;
const MOCK_DATA = {
  "/api/status": {
    version: "0.1.0",
    uptime_ms: 86400000,
    alive: 3,
    cluster_auth: true,
    plaintext_allowed: false,
    crypto: ["aes-256-gcm", "chacha20-poly1305"],
    node: { id: 1, region: "cn-east", addr: "10.0.0.1:7946", load_permille: 120, sessions: 3 },
    members: [
      { id: 2, region: "cn-north", addr: "10.0.0.2:7946", load_permille: 760, sessions: 41, state: "alive", idle_ms: 310 },
      { id: 3, region: "us-west", addr: "10.0.1.3:7946", load_permille: 940, sessions: 88, state: "suspect", idle_ms: 4200 },
      { id: 4, region: "eu-west", addr: "10.0.2.4:7946", load_permille: 0, sessions: 0, state: "dead", idle_ms: 93000 },
    ],
  },
  "/api/cluster": {
    self: 1,
    generated_ms: NOW,
    nodes: [
      { id: 1, region: "cn-east", role: "paid", state: "alive", addr: "10.0.0.1:7946", signal: "203.0.113.1:4433", relay: "203.0.113.1:3478", load_permille: 120, sessions: 3, self: true },
      { id: 2, region: "cn-north", role: "paid", state: "alive", addr: "10.0.0.2:7946", signal: "203.0.113.2:4433", relay: "203.0.113.2:3478", load_permille: 760, sessions: 41 },
      { id: 3, region: "us-west", role: "free", state: "suspect", addr: "10.0.1.3:7946", signal: "198.51.100.3:4433", relay: "198.51.100.3:3478", load_permille: 940, sessions: 88 },
      { id: 4, region: "eu-west", role: "free", state: "dead", addr: "10.0.2.4:7946", signal: "198.51.100.4:4433", relay: "198.51.100.4:3478", load_permille: 0, sessions: 0 },
    ],
  },
  "/api/usage": {
    month: 202611,
    node: 1,
    price_cents_per_gb: 80,
    accounts: [
      { account: "abcdef0123456789", bytes: 123456789, sessions: 2, cents: 10, cluster_bytes: 456789012 },
      { account: "1122334455667788", bytes: 85899345920, sessions: 17, cents: 6400, cluster_bytes: 214748364800 },
      { account: "0000000000000000", bytes: 5368709120, sessions: 52, cents: 0, cluster_bytes: 9663676416 },
    ],
    free: { window_secs: 600, limit_bytes: 200000000, limited_now: 2 },
  },
};

export async function getJson(path) {
  if (MOCK) {
    const key = path.split("?")[0];
    if (key === "/api/route") return { node: { id: 1, region: "cn-east" } };
    return MOCK_DATA[key];
  }
  const res = await fetch(path);
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
}

// 管理操作；返回 {ok, body} 或 {ok:false, error}，错误信息翻译成中文。
export async function postAdmin(path, token, payload) {
  if (MOCK) {
    return path === "/api/accounts"
      ? { ok: true, body: { account: "abcdef0123456789", token: "pdk1-MOCK-TOKEN-示例" } }
      : { ok: true, body: { ok: true } };
  }
  let res;
  try {
    res = await fetch(path, {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: `Bearer ${token}` },
      body: JSON.stringify(payload),
    });
  } catch (err) {
    return { ok: false, error: `无法连接服务器：${err}` };
  }
  let body = null;
  try {
    body = await res.json();
  } catch {
    /* 非 JSON 响应 */
  }
  if (res.ok) return { ok: true, body };
  if (res.status === 401) return { ok: false, error: "管理令牌不对" };
  const detail = body && body.error ? `：${body.error}` : "";
  return { ok: false, error: `请求失败（HTTP ${res.status}）${detail}` };
}

// 每 5 秒刷新一次；失败时保留上一次数据并带上错误，不会白屏。
export function usePolled(path) {
  const [state, setState] = useState({ data: null, error: null, loading: true });
  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const data = await getJson(path);
        if (!cancelled) setState({ data, error: null, loading: false });
      } catch (err) {
        if (!cancelled) setState((p) => ({ data: p.data, error: String(err), loading: false }));
      }
    };
    setState((p) => ({ ...p, loading: true }));
    load();
    const timer = setInterval(load, REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [path]);
  return state;
}

export function formatBytes(n) {
  if (!Number.isFinite(n)) return "–";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i += 1;
  }
  return `${i === 0 ? v : v.toFixed(v >= 100 ? 0 : 2)} ${units[i]}`;
}

export function formatYuan(cents) {
  return `¥${(cents / 100).toFixed(2)}`;
}

export function ErrorLine({ error, what }) {
  return error ? (
    <p className="error">
      {what}获取失败：{error}（每 5 秒自动重试）
    </p>
  ) : null;
}
