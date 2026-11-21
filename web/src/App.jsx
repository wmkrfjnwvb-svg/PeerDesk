import { useEffect, useState } from "react";
import { getJson, MOCK, REFRESH_MS } from "./api.jsx";
import Admin from "./Admin.jsx";
import Cluster, { Load } from "./Cluster.jsx";
import Usage from "./Usage.jsx";

function useStatus() {
  const [state, setState] = useState({ status: null, error: null });
  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const status = await getJson("/api/status");
        if (!cancelled) setState({ status, error: null });
      } catch (err) {
        if (!cancelled) setState((prev) => ({ status: prev.status, error: String(err) }));
      }
    };
    load();
    const timer = setInterval(load, REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);
  return state;
}

function NodeRow({ node, state, idleMs, self }) {
  return (
    <tr>
      <td>
        {node.id}
        {self && <span className="tag">本页所在节点</span>}
      </td>
      <td>{node.region}</td>
      <td className="mono">{node.addr}</td>
      <td>
        <span className={`state ${state}`}>{state}</span>
      </td>
      <td>
        <Load permille={node.load_permille} />
      </td>
      <td className="num">{node.sessions}</td>
      <td className="num">{idleMs == null ? "–" : `${idleMs} ms`}</td>
    </tr>
  );
}

function RouteTester() {
  const [region, setRegion] = useState("");
  const [result, setResult] = useState(null);
  const submit = async (event) => {
    event.preventDefault();
    if (!region) return;
    try {
      const body = await getJson(`/api/route?region=${encodeURIComponent(region)}`);
      setResult(body.node ? `节点 ${body.node.id}（${body.node.region}）` : body.error);
    } catch (err) {
      setResult(String(err));
    }
  };
  return (
    <form className="route" onSubmit={submit}>
      <label htmlFor="region">哪个节点服务该区域的客户端</label>
      <input id="region" value={region} onChange={(e) => setRegion(e.target.value.trim())} placeholder="us-east" />
      <button type="submit">查询</button>
      {result && <output>{result}</output>}
    </form>
  );
}

function StatusView() {
  const { status, error } = useStatus();
  if (!status) {
    return (
      <section>
        {error ? <p className="error">无法连接服务器：{error}（每 5 秒自动重试）</p> : <p className="muted">加载中…</p>}
      </section>
    );
  }
  const total = status.members.length + 1;
  return (
    <>
      <section>
        <p className="meta">
          v{status.version} · 已运行 {Math.round(status.uptime_ms / 1000)} 秒 · {status.alive} / {total} 台节点存活
        </p>
        {error && <p className="error">上次刷新失败：{error}</p>}
        <h2>节点状态</h2>
        <div className="scroll">
          <table>
            <thead>
              <tr>
                <th>节点</th>
                <th>区域</th>
                <th>地址</th>
                <th>状态</th>
                <th>负载</th>
                <th className="num">会话</th>
                <th className="num">最近心跳</th>
              </tr>
            </thead>
            <tbody>
              <NodeRow node={status.node} state="alive" idleMs={null} self />
              {status.members.map((m) => (
                <NodeRow key={m.id} node={m} state={m.state} idleMs={m.idle_ms} />
              ))}
            </tbody>
          </table>
        </div>
        <RouteTester />
      </section>

      <section>
        <h2>加密策略</h2>
        <p>
          加密套件（按优先级）：<span className="mono">{status.crypto.join(", ")}</span>
        </p>
        {!status.cluster_auth && (
          <p className="error">
            集群心跳未认证。请在每个节点上设置 <span className="mono">cluster_secret</span>。
          </p>
        )}
        {status.plaintext_allowed && <p className="error">当前允许明文会话，仅可在可信内网使用。</p>}
      </section>
    </>
  );
}

const TABS = [
  ["cluster", "集群总览"],
  ["usage", "中继用量与计费"],
  ["admin", "管理操作"],
  ["status", "本节点状态"],
];

export default function App() {
  const [tab, setTab] = useState(() => {
    try {
      return sessionStorage.getItem("peerdesk.tab") || "cluster";
    } catch {
      return "cluster";
    }
  });
  const pick = (k) => {
    setTab(k);
    try {
      sessionStorage.setItem("peerdesk.tab", k);
    } catch {
      /* 忽略 */
    }
  };
  return (
    <main>
      <header>
        <h1>PeerDesk 管理中心</h1>
        {MOCK && <p className="meta">演示数据（?mock=1），未连接服务器</p>}
      </header>
      <nav className="tabs" role="tablist">
        {TABS.map(([k, label]) => (
          <button key={k} role="tab" aria-selected={tab === k} className={tab === k ? "on" : ""} onClick={() => pick(k)}>
            {label}
          </button>
        ))}
      </nav>
      {tab === "cluster" && <Cluster />}
      {tab === "usage" && <Usage />}
      {tab === "admin" && <Admin />}
      {tab === "status" && <StatusView />}
    </main>
  );
}
