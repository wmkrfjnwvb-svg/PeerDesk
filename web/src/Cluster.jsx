import { ErrorLine, usePolled } from "./api.jsx";

const STATES = [
  ["alive", "存活"],
  ["suspect", "可疑"],
  ["dead", "离线"],
];
const ROLES = [
  ["paid", "付费集群"],
  ["free", "免费集群"],
];

export function Load({ permille }) {
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

function Counts({ nodes }) {
  return (
    <span className="counts">
      共 {nodes.length} 台
      {STATES.map(([k, label]) => {
        const n = nodes.filter((x) => x.state === k).length;
        return n > 0 || k === "alive" ? (
          <span key={k} className={`state ${k}`}>
            {label} {n}
          </span>
        ) : null;
      })}
    </span>
  );
}

function RoleTable({ title, nodes }) {
  return (
    <div className="group">
      <h3>
        {title} <Counts nodes={nodes} />
      </h3>
      {nodes.length === 0 ? (
        <p className="muted">暂无节点</p>
      ) : (
        <div className="scroll">
          <table>
            <thead>
              <tr>
                <th>节点</th>
                <th>区域</th>
                <th>状态</th>
                <th>集群地址</th>
                <th>信令</th>
                <th>中继</th>
                <th>负载</th>
                <th className="num">会话</th>
              </tr>
            </thead>
            <tbody>
              {nodes.map((n) => (
                <tr key={n.id} className={`row-${n.state}`}>
                  <td>
                    {n.id}
                    {n.self && <span className="tag">本页所在节点</span>}
                  </td>
                  <td>{n.region}</td>
                  <td>
                    <span className={`state ${n.state}`}>{STATES.find(([k]) => k === n.state)?.[1] ?? n.state}</span>
                  </td>
                  <td className="mono">{n.addr}</td>
                  <td className="mono">{n.signal}</td>
                  <td className="mono">{n.relay}</td>
                  <td>
                    <Load permille={n.load_permille} />
                  </td>
                  <td className="num">{n.sessions}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

export default function Cluster() {
  const { data, error, loading } = usePolled("/api/cluster");
  const nodes = data?.nodes ?? [];
  const known = ROLES.map(([k]) => k);
  const other = nodes.filter((n) => !known.includes(n.role));
  return (
    <section>
      <h2>集群总览</h2>
      <ErrorLine error={error} what="集群信息" />
      {!data && loading && <p className="muted">加载中…</p>}
      {data && (
        <>
          <p className="summary">
            <Counts nodes={nodes} />
          </p>
          {ROLES.map(([k, label]) => (
            <RoleTable key={k} title={label} nodes={nodes.filter((n) => n.role === k)} />
          ))}
          {other.length > 0 && <RoleTable title="未分类" nodes={other} />}
        </>
      )}
    </section>
  );
}
