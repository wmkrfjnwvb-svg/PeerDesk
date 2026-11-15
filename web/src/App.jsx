import { useEffect, useState } from "react";

const REFRESH_MS = 2000;

function useStatus() {
  const [state, setState] = useState({ status: null, error: null });
  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const res = await fetch("/api/status");
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const status = await res.json();
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

function Load({ permille }) {
  const pct = Math.round(permille / 10);
  const level = permille >= 900 ? "bad" : permille >= 700 ? "warn" : "ok";
  return (
    <span className="load">
      <span className="bar">
        <span className={`fill ${level}`} style={{ width: `${Math.min(pct, 100)}%` }} />
      </span>
      {pct}%
    </span>
  );
}

function NodeRow({ node, state, idleMs, self }) {
  return (
    <tr>
      <td>
        {node.id}
        {self && <span className="tag">this node</span>}
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
      const res = await fetch(`/api/route?region=${encodeURIComponent(region)}`);
      const body = await res.json();
      setResult(body.node ? `node ${body.node.id} (${body.node.region})` : body.error);
    } catch (err) {
      setResult(String(err));
    }
  };
  return (
    <form className="route" onSubmit={submit}>
      <label htmlFor="region">Which node serves a client in region</label>
      <input
        id="region"
        value={region}
        onChange={(e) => setRegion(e.target.value.trim())}
        placeholder="us-east"
      />
      <button type="submit">Check</button>
      {result && <output>{result}</output>}
    </form>
  );
}

export default function App() {
  const { status, error } = useStatus();
  if (!status) {
    return <main>{error ? <p className="error">Cannot reach server: {error}</p> : <p>Loading…</p>}</main>;
  }
  const total = status.members.length + 1;
  return (
    <main>
      <header>
        <h1>PeerDesk Console</h1>
        <p>
          v{status.version} · up {Math.round(status.uptime_ms / 1000)} s · {status.alive} of {total} nodes alive
        </p>
        {error && <p className="error">Last refresh failed: {error}</p>}
      </header>

      <section>
        <h2>Cluster</h2>
        <div className="scroll">
          <table>
            <thead>
              <tr>
                <th>Node</th>
                <th>Region</th>
                <th>Address</th>
                <th>State</th>
                <th>Load</th>
                <th className="num">Sessions</th>
                <th className="num">Last heartbeat</th>
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
        <h2>Encryption policy</h2>
        <p>
          Suites in order of preference: <span className="mono">{status.crypto.join(", ")}</span>
        </p>
        {!status.cluster_auth && (
          <p className="error">
            Cluster heartbeats are not authenticated. Set <span className="mono">cluster_secret</span> on every node.
          </p>
        )}
        {status.plaintext_allowed && (
          <p className="error">Unencrypted sessions are allowed. Use this only on a trusted private network.</p>
        )}
      </section>
    </main>
  );
}
