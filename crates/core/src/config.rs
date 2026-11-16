//! 客户端与服务端的配置文件（TOML）。未知字段一律报错，避免拼写错误被悄悄忽略。

use serde::Deserialize;
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::crypto::{Policy, Suite};
use crate::proto::MAX_REGION_LEN;
use crate::region::{is_lan_ip, Candidate, NodeId};
use crate::Error;

/// 加密配置。`preset` 与 `suites` 二选一，都不写时用推荐预设。
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CryptoConfig {
    /// 预设：`recommended`、`fips`、`none`。
    #[serde(default)]
    pub preset: Option<String>,
    /// 自定义套件列表，按偏好排列。
    #[serde(default)]
    pub suites: Vec<String>,
}

impl CryptoConfig {
    /// 解析成策略。
    pub fn policy(&self) -> Result<Policy, Error> {
        match (&self.preset, self.suites.is_empty()) {
            (Some(_), false) => Err(Error::Config(
                "crypto.preset 与 crypto.suites 只能写一个".into(),
            )),
            (Some(preset), true) => Policy::preset(preset),
            (None, true) => Ok(Policy::recommended()),
            (None, false) => {
                let suites = self
                    .suites
                    .iter()
                    .map(|name| Suite::parse(name))
                    .collect::<Result<Vec<_>, _>>()?;
                Policy::custom(&suites)
            }
        }
    }
}

fn check_region(field: &str, region: &str) -> Result<(), Error> {
    if region.is_empty() || region.len() > MAX_REGION_LEN {
        return Err(Error::Config(format!(
            "{field} 必须是 1 到 {MAX_REGION_LEN} 字节的区域名"
        )));
    }
    Ok(())
}

fn check_addr(field: &str, addr: &str) -> Result<SocketAddr, Error> {
    addr.parse()
        .map_err(|_| Error::Config(format!("{field} 不是合法的 地址:端口：{addr}")))
}

/// 客户端配置里的一台服务器。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerEntry {
    /// 地址，`IP:端口`。
    pub addr: String,
    /// 所在区域。
    pub region: String,
    /// 强制视为内网。地址本身是内网地址时会自动识别，无需设置。
    #[serde(default)]
    pub lan: bool,
}

/// 客户端配置。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    /// 本机所在区域。
    pub region: String,
    /// 本区域没有可用服务器时，依次尝试的区域。
    #[serde(default)]
    pub region_fallback: Vec<String>,
    /// 可用的服务器。
    #[serde(default)]
    pub servers: Vec<ServerEntry>,
    /// 加密配置。
    #[serde(default)]
    pub crypto: CryptoConfig,
}

impl ClientConfig {
    /// 解析并校验。
    pub fn from_toml(text: &str) -> Result<Self, Error> {
        let cfg: Self = toml::from_str(text).map_err(|e| Error::Config(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), Error> {
        check_region("region", &self.region)?;
        for region in &self.region_fallback {
            check_region("region_fallback", region)?;
        }
        for server in &self.servers {
            check_region("servers.region", &server.region)?;
            check_addr("servers.addr", &server.addr)?;
        }
        self.crypto.policy().map(|_| ())
    }

    /// 把服务器列表转成候选节点，节点标识是它在列表中的序号。内网地址自动标记。
    #[must_use]
    pub fn candidates(&self) -> Vec<Candidate> {
        self.servers
            .iter()
            .zip(0u64..)
            .map(|(server, index)| Candidate {
                id: NodeId(index),
                region: server.region.clone(),
                lan: server.lan
                    || server
                        .addr
                        .parse::<SocketAddr>()
                        .is_ok_and(|a| is_lan_ip(a.ip())),
                rtt_ms: None,
                load_permille: 0,
                healthy: true,
            })
            .collect()
    }
}

/// 服务端配置。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// 节点标识，集群内唯一，不能为 0。
    pub node_id: u64,
    /// 所在区域。
    pub region: String,
    /// 管理页面与接口的监听地址。
    pub http_listen: String,
    /// 集群心跳的监听地址（UDP）。
    pub cluster_listen: String,
    /// 其他节点用来联系本节点的地址。
    pub advertise: String,
    /// 启动时已知的其他节点地址。
    #[serde(default)]
    pub peers: Vec<String>,
    /// 集群共享密钥。设置后节点之间的心跳带认证，没有这个密钥的机器无法加入或干扰集群。
    /// 所有节点必须相同，至少 16 个字符。不设置时心跳没有认证，只能用于受信任的内网。
    #[serde(default)]
    pub cluster_secret: Option<String>,
    /// 本区域过载时，客户端依次溢出到的区域。
    #[serde(default)]
    pub region_fallback: Vec<String>,
    /// 信令的监听地址（UDP，QUIC）。不写时不提供信令。
    #[serde(default)]
    pub signal_listen: Option<String>,
    /// 中继的监听地址（UDP）。需要同时开启信令。
    #[serde(default)]
    pub relay_listen: Option<String>,
    /// 告诉客户端的中继地址（服务器在 NAT 或负载均衡后面时填公网地址）。不写时用实际监听地址。
    #[serde(default)]
    pub relay_advertise: Option<String>,
    /// 保存服务器身份（证书和私钥）的目录。开启信令时必须设置。
    #[serde(default)]
    pub data_dir: Option<String>,
    /// 加密配置。
    #[serde(default)]
    pub crypto: CryptoConfig,
}

/// 解析好的信令配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalConfig {
    /// 信令的监听地址。
    pub listen: SocketAddr,
    /// 保存服务器身份的目录。
    pub data_dir: PathBuf,
    /// 中继；`None` 表示不提供。
    pub relay: Option<RelayConfig>,
}

/// 解析好的中继配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayConfig {
    /// 监听地址。
    pub listen: SocketAddr,
    /// 告诉客户端的地址；`None` 表示用实际监听地址。
    pub advertise: Option<SocketAddr>,
}

impl ServerConfig {
    /// 信令和中继的配置。没有开启信令时返回 `None`。
    pub fn signal(&self) -> Result<Option<SignalConfig>, Error> {
        let addr = |field: &str, value: &Option<String>| {
            value.as_deref().map(|a| check_addr(field, a)).transpose()
        };
        let relay = match (
            addr("relay_listen", &self.relay_listen)?,
            addr("relay_advertise", &self.relay_advertise)?,
        ) {
            (Some(listen), advertise) => Some(RelayConfig { listen, advertise }),
            (None, Some(_)) => {
                return Err(Error::Config("relay_advertise 需要 relay_listen".into()))
            }
            (None, None) => None,
        };
        let Some(listen) = addr("signal_listen", &self.signal_listen)? else {
            return match relay {
                Some(_) => Err(Error::Config("relay_listen 需要 signal_listen".into())),
                None => Ok(None),
            };
        };
        let data_dir = self
            .data_dir
            .as_ref()
            .ok_or_else(|| Error::Config("开启信令时必须设置 data_dir（保存服务器身份）".into()))?;
        Ok(Some(SignalConfig {
            listen,
            data_dir: PathBuf::from(data_dir),
            relay,
        }))
    }

    /// 解析并校验。
    pub fn from_toml(text: &str) -> Result<Self, Error> {
        let cfg: Self = toml::from_str(text).map_err(|e| Error::Config(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), Error> {
        if self.node_id == 0 {
            return Err(Error::Config("node_id 不能为 0".into()));
        }
        check_region("region", &self.region)?;
        for region in &self.region_fallback {
            check_region("region_fallback", region)?;
        }
        check_addr("http_listen", &self.http_listen)?;
        check_addr("cluster_listen", &self.cluster_listen)?;
        check_addr("advertise", &self.advertise)?;
        for peer in &self.peers {
            check_addr("peers", peer)?;
        }
        if self
            .cluster_secret
            .as_ref()
            .is_some_and(|s| s.len() < crate::cluster::MIN_SECRET_LEN)
        {
            return Err(Error::Config(format!(
                "cluster_secret 至少 {} 个字符",
                crate::cluster::MIN_SECRET_LEN
            )));
        }
        self.signal()?;
        self.crypto.policy().map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT: &str = r#"
region = "us-east"
region_fallback = ["us-west", "eu"]

[[servers]]
addr = "203.0.113.10:4433"
region = "us-east"

[[servers]]
addr = "192.168.1.20:4433"
region = "office"

[[servers]]
addr = "198.51.100.7:4433"
region = "eu"
lan = true

[crypto]
suites = ["chacha20-poly1305", "aes-256-gcm"]
"#;

    const SERVER: &str = r#"
node_id = 1
region = "us-east"
http_listen = "0.0.0.0:8080"
cluster_listen = "0.0.0.0:7946"
advertise = "10.0.0.1:7946"
peers = ["10.0.0.2:7946"]
region_fallback = ["us-west"]

[crypto]
preset = "fips"
"#;

    #[test]
    fn client_config_parses() {
        let cfg = ClientConfig::from_toml(CLIENT).unwrap();
        assert_eq!(cfg.region, "us-east");
        assert_eq!(
            cfg.crypto.policy().unwrap().suites(),
            &[Suite::ChaCha20Poly1305, Suite::Aes256Gcm]
        );
        let c = cfg.candidates();
        assert_eq!(c.len(), 3);
        assert!(!c[0].lan);
        assert!(c[1].lan, "内网地址自动识别");
        assert!(c[2].lan, "配置里强制指定");
        assert_eq!(c[2].id, NodeId(2));
    }

    #[test]
    fn server_config_parses() {
        let cfg = ServerConfig::from_toml(SERVER).unwrap();
        assert_eq!(cfg.node_id, 1);
        assert_eq!(cfg.peers, vec!["10.0.0.2:7946"]);
        assert_eq!(cfg.crypto.policy().unwrap(), Policy::fips());
    }

    #[test]
    fn minimal_configs_use_defaults() {
        let cfg = ClientConfig::from_toml("region = \"x\"").unwrap();
        assert!(cfg.servers.is_empty());
        assert_eq!(cfg.crypto.policy().unwrap(), Policy::recommended());
        assert!(cfg.candidates().is_empty());
    }

    fn client_err(text: &str) -> String {
        let err = ClientConfig::from_toml(text).unwrap_err();
        assert!(matches!(err, Error::Config(_)), "{err:?}");
        err.to_string()
    }

    fn server_err(text: &str) -> String {
        let err = ServerConfig::from_toml(text).unwrap_err();
        assert!(matches!(err, Error::Config(_)), "{err:?}");
        err.to_string()
    }

    #[test]
    fn client_config_errors() {
        assert!(!client_err("not toml =").is_empty());
        assert!(!client_err("").is_empty()); // 缺少 region
        assert!(client_err("region = \"\"").contains("region"));
        assert!(client_err(&format!("region = \"{}\"", "r".repeat(33))).contains("region"));
        assert!(client_err("region = \"x\"\ntypo = 1").contains("typo"));
        assert!(client_err("region = \"x\"\nregion_fallback = [\"\"]").contains("region_fallback"));
        assert!(
            client_err("region = \"x\"\n[[servers]]\naddr = \"nope\"\nregion = \"x\"")
                .contains("servers.addr")
        );
        assert!(
            client_err("region = \"x\"\n[[servers]]\naddr = \"1.2.3.4:1\"\nregion = \"\"")
                .contains("servers.region")
        );
        assert!(
            client_err("region = \"x\"\n[crypto]\npreset = \"fips\"\nsuites = [\"none\"]")
                .contains("只能写一个")
        );
        assert!(client_err("region = \"x\"\n[crypto]\npreset = \"weird\"").contains("weird"));
        assert_eq!(
            ClientConfig::from_toml("region = \"x\"\n[crypto]\nsuites = [\"rot13\"]"),
            Err(Error::UnknownSuiteName("rot13".into()))
        );
    }

    #[test]
    fn server_config_errors() {
        assert!(!server_err("node_id = \"one\"").is_empty());
        assert!(server_err(&SERVER.replace("node_id = 1", "node_id = 0")).contains("node_id"));
        assert!(server_err(&SERVER.replace("\"us-east\"", "\"\"")).contains("region"));
        assert!(server_err(&SERVER.replace("[\"us-west\"]", "[\"\"]")).contains("region_fallback"));
        assert!(server_err(&SERVER.replace("0.0.0.0:8080", "x")).contains("http_listen"));
        assert!(server_err(&SERVER.replace("0.0.0.0:7946", "x")).contains("cluster_listen"));
        assert!(server_err(&SERVER.replace("10.0.0.1:7946", "x")).contains("advertise"));
        assert!(server_err(&SERVER.replace("10.0.0.2:7946", "x")).contains("peers"));
        assert!(server_err(&SERVER.replace("fips", "nope")).contains("nope"));
        let short = format!("cluster_secret = \"short\"\n{SERVER}");
        assert!(server_err(&short).contains("cluster_secret"));
        let ok = format!("cluster_secret = \"0123456789abcdef\"\n{SERVER}");
        assert_eq!(
            ServerConfig::from_toml(&ok)
                .unwrap()
                .cluster_secret
                .as_deref(),
            Some("0123456789abcdef")
        );
    }

    #[test]
    fn signal_and_relay_config() {
        // 不写信令：不开启，data_dir 单独写也无妨
        let cfg = ServerConfig::from_toml(&format!("data_dir = \"d\"\n{SERVER}")).unwrap();
        assert_eq!(cfg.signal().unwrap(), None);
        // 只有信令
        let only =
            format!("signal_listen = \"0.0.0.0:4433\"\ndata_dir = \"/var/lib/pd\"\n{SERVER}");
        let signal = ServerConfig::from_toml(&only)
            .unwrap()
            .signal()
            .unwrap()
            .unwrap();
        assert_eq!(signal.listen, "0.0.0.0:4433".parse().unwrap());
        assert_eq!(signal.data_dir, PathBuf::from("/var/lib/pd"));
        assert_eq!(signal.relay, None);
        // 信令加中继，中继有对外地址
        let full = format!(
            "relay_listen = \"0.0.0.0:3478\"\nrelay_advertise = \"203.0.113.5:3478\"\n{only}"
        );
        let relay = ServerConfig::from_toml(&full)
            .unwrap()
            .signal()
            .unwrap()
            .unwrap()
            .relay;
        assert_eq!(
            relay,
            Some(RelayConfig {
                listen: "0.0.0.0:3478".parse().unwrap(),
                advertise: Some("203.0.113.5:3478".parse().unwrap()),
            })
        );
        // 错误
        let no_dir = format!("signal_listen = \"0.0.0.0:4433\"\n{SERVER}");
        assert!(server_err(&no_dir).contains("data_dir"));
        assert!(server_err(&only.replace("0.0.0.0:4433", "x")).contains("signal_listen"));
        assert!(server_err(&full.replace("0.0.0.0:3478", "x")).contains("relay_listen"));
        assert!(server_err(&full.replace("203.0.113.5:3478", "x")).contains("relay_advertise"));
        let relay_alone = format!("relay_listen = \"0.0.0.0:3478\"\n{SERVER}");
        assert!(server_err(&relay_alone).contains("relay_listen 需要 signal_listen"));
        // 仓库里的示例配置必须有效
        let example = include_str!("../../../config/server.example.toml");
        let signal = ServerConfig::from_toml(example)
            .unwrap()
            .signal()
            .unwrap()
            .unwrap();
        assert!(signal.relay.unwrap().advertise.is_some());
        assert!(
            ClientConfig::from_toml(include_str!("../../../config/client.example.toml")).is_ok()
        );
        let advertise_alone = format!("relay_advertise = \"1.2.3.4:5\"\n{SERVER}");
        assert!(server_err(&advertise_alone).contains("relay_advertise 需要 relay_listen"));
    }
}
