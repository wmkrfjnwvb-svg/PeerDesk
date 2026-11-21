import { useState } from "react";
import { postAdmin } from "./api.jsx";

const KEY = "peerdesk.adminToken";

function readToken() {
  try {
    return sessionStorage.getItem(KEY) || "";
  } catch {
    return "";
  }
}
function writeToken(v) {
  try {
    sessionStorage.setItem(KEY, v);
  } catch {
    /* 存储不可用时只在内存里保留 */
  }
}

export default function Admin() {
  const [token, setToken] = useState(readToken());
  const [addr, setAddr] = useState("");
  const [quota, setQuota] = useState("0");
  const [peerMsg, setPeerMsg] = useState(null);
  const [issued, setIssued] = useState(null);
  const [issueErr, setIssueErr] = useState(null);
  const [copied, setCopied] = useState(false);
  const [busy, setBusy] = useState(false);

  const changeToken = (v) => {
    setToken(v);
    writeToken(v);
  };

  const addPeer = async (e) => {
    e.preventDefault();
    if (!token) return setPeerMsg({ error: "请先填写管理令牌" });
    if (!addr.trim()) return setPeerMsg({ error: "请填写节点地址，例如 10.0.0.2:7946" });
    setBusy(true);
    const r = await postAdmin("/api/peers", token, { addr: addr.trim() });
    setBusy(false);
    setPeerMsg(r.ok ? { ok: `已加入节点 ${addr.trim()}` } : { error: r.error });
    if (r.ok) setAddr("");
  };

  const issue = async (e) => {
    e.preventDefault();
    setIssued(null);
    setCopied(false);
    if (!token) return setIssueErr("请先填写管理令牌");
    const gb = Number(quota);
    if (!Number.isFinite(gb) || gb < 0) return setIssueErr("流量额度必须是不小于 0 的数字");
    setBusy(true);
    const r = await postAdmin("/api/accounts", token, { quota_gb: gb });
    setBusy(false);
    if (r.ok && r.body?.token) {
      setIssueErr(null);
      setIssued(r.body);
    } else {
      setIssueErr(r.ok ? "服务器返回的数据不完整" : r.error);
    }
  };

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(issued.token);
      setCopied(true);
    } catch {
      setCopied(false);
      setIssueErr("复制失败，请手动选中令牌复制");
    }
  };

  return (
    <section>
      <h2>管理操作</h2>
      <div className="field">
        <label htmlFor="admin-token">管理令牌</label>
        <input
          id="admin-token"
          type="password"
          autoComplete="off"
          value={token}
          onChange={(e) => changeToken(e.target.value)}
          placeholder="只保存在本标签页，关闭即清除"
        />
      </div>

      <form className="action" onSubmit={addPeer}>
        <h3>加入节点</h3>
        <div className="row">
          <input value={addr} onChange={(e) => setAddr(e.target.value)} placeholder="host:port，如 10.0.0.2:7946" />
          <button type="submit" disabled={busy}>
            加入
          </button>
        </div>
        {peerMsg?.ok && <p className="okmsg">{peerMsg.ok}</p>}
        {peerMsg?.error && <p className="error">{peerMsg.error}</p>}
      </form>

      <form className="action" onSubmit={issue}>
        <h3>签发会员令牌</h3>
        <div className="row">
          <label htmlFor="quota">流量额度（GB，0 = 不限）</label>
          <input id="quota" type="number" min="0" step="any" value={quota} onChange={(e) => setQuota(e.target.value)} />
          <button type="submit" disabled={busy}>
            签发
          </button>
        </div>
        {issueErr && <p className="error">{issueErr}</p>}
        {issued && (
          <div className="issued">
            <p>
              账户：<span className="mono">{issued.account}</span>
            </p>
            <div className="row">
              <code className="token">{issued.token}</code>
              <button type="button" onClick={copy}>
                {copied ? "已复制" : "复制"}
              </button>
            </div>
            <p className="error">令牌只显示这一次，离开或刷新页面后无法找回，请立即保存。</p>
          </div>
        )}
      </form>
    </section>
  );
}
