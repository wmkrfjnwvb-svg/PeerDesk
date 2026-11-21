import { useState } from "react";
import { ErrorLine, formatBytes, formatYuan, usePolled } from "./api.jsx";

const FREE_ACCOUNT = "0000000000000000";

function currentMonth() {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}`;
}

export default function Usage() {
  const [month, setMonth] = useState(currentMonth());
  const yyyymm = month ? month.replace("-", "") : "";
  const { data, error, loading } = usePolled(`/api/usage${yyyymm ? `?month=${yyyymm}` : ""}`);
  const accounts = data?.accounts ?? [];
  const sum = (key) => accounts.reduce((a, x) => a + (x[key] ?? 0), 0);
  const free = data?.free;
  return (
    <section>
      <h2>中继用量与计费</h2>
      <div className="toolbar">
        <label>
          月份 <input type="month" value={month} onChange={(e) => setMonth(e.target.value)} />
        </label>
        {data && (
          <span className="muted">
            本节点 {data.node} · 单价 {formatYuan(data.price_cents_per_gb)}/GB
          </span>
        )}
      </div>
      <ErrorLine error={error} what="用量" />
      {!data && loading && <p className="muted">加载中…</p>}
      {data && (
        <>
          <div className="scroll">
            <table>
              <thead>
                <tr>
                  <th>账户</th>
                  <th className="num">本节点流量</th>
                  <th className="num">全集群流量</th>
                  <th className="num">会话数</th>
                  <th className="num">金额（本节点）</th>
                </tr>
              </thead>
              <tbody>
                {accounts.length === 0 && (
                  <tr>
                    <td colSpan={5} className="muted">
                      本月暂无中继用量
                    </td>
                  </tr>
                )}
                {accounts.map((a) => (
                  <tr key={a.account}>
                    <td>{a.account === FREE_ACCOUNT ? "免费用户（公共）" : <span className="mono">{a.account}</span>}</td>
                    <td className="num">{formatBytes(a.bytes)}</td>
                    <td className="num">{formatBytes(a.cluster_bytes)}</td>
                    <td className="num">{a.sessions}</td>
                    <td className="num">{formatYuan(a.cents)}</td>
                  </tr>
                ))}
              </tbody>
              {accounts.length > 0 && (
                <tfoot>
                  <tr>
                    <th>合计</th>
                    <th className="num">{formatBytes(sum("bytes"))}</th>
                    <th className="num">{formatBytes(sum("cluster_bytes"))}</th>
                    <th className="num">{sum("sessions")}</th>
                    <th className="num">{formatYuan(sum("cents"))}</th>
                  </tr>
                </tfoot>
              )}
            </table>
          </div>
          {free && (
            <p className="policy">
              免费集群：每台客户端每 {Math.round(free.window_secs / 60)} 分钟 {formatBytes(free.limit_bytes)}；当前被限流{" "}
              {free.limited_now} 台
            </p>
          )}
        </>
      )}
    </section>
  );
}
