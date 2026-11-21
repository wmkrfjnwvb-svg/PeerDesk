//! 极小的 HTTP/1.1 处理：只做请求行解析和路由，不做 IO，便于测试。
//!
//! 服务管理中心页面和接口。写操作（加入节点、签发会员令牌）要带管理令牌。
//! 每个连接处理一个请求后关闭。

use std::borrow::Cow;

/// 请求头的最大长度；超过即拒绝。
pub const MAX_REQUEST_HEAD: usize = 8 * 1024;

/// 解析出的请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// 方法。
    pub method: String,
    /// 路径，不含查询串。
    pub path: String,
    /// 查询串，不含问号；没有时为空。
    pub query: String,
    /// `Authorization: Bearer <令牌>` 里的令牌。
    pub token: Option<String>,
    /// 请求体（最多 [`MAX_BODY`] 字节）。
    pub body: String,
}

/// 请求体的上限。
pub const MAX_BODY: usize = 4 * 1024;

/// 请求头里的 `Content-Length`（没有时为 0）。
#[must_use]
pub fn content_length(head: &str) -> usize {
    head.lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0)
}

/// 从一个很小的 JSON 对象里取一个字段的值（字符串去掉引号；数字原样）。
/// 只用于管理接口里形如 `{"addr":"1.2.3.4:7946"}`、`{"quota_gb":50}` 的请求体。
#[must_use]
pub fn json_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let after = body.split_once(needle.as_str())?.1;
    let value = after.trim_start().strip_prefix(':')?.trim_start();
    if let Some(rest) = value.strip_prefix('"') {
        return rest.split_once('"').map(|(v, _)| v);
    }
    let end = value.find([',', '}']).unwrap_or(value.len());
    Some(value.get(..end)?.trim())
}

/// 解析请求行（请求头的第一行）。
#[must_use]
pub fn parse_request(head: &str) -> Option<Request> {
    let line = head.lines().next()?;
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return None;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let (head_part, body) = head.split_once("\r\n\r\n").unwrap_or((head, ""));
    let token = head_part
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("authorization"))
        .and_then(|(_, v)| v.trim().strip_prefix("Bearer "))
        .map(|t| t.trim().to_owned());
    let limit = content_length(head_part).min(MAX_BODY).min(body.len());
    Some(Request {
        method: method.to_owned(),
        path: path.to_owned(),
        query: query.to_owned(),
        token,
        body: body.get(..limit).unwrap_or_default().to_owned(),
    })
}

/// 取查询串里某个参数的值。不做百分号解码：区域名只允许简单字符。
#[must_use]
pub fn query_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

/// 响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// 状态码。
    pub status: u16,
    /// 内容类型。
    pub content_type: &'static str,
    /// 响应体。
    pub body: Cow<'static, [u8]>,
}

impl Response {
    /// JSON 响应。
    #[must_use]
    pub fn json(status: u16, body: String) -> Self {
        Self {
            status,
            content_type: "application/json; charset=utf-8",
            body: Cow::Owned(body.into_bytes()),
        }
    }

    /// 纯文本响应。
    #[must_use]
    pub const fn text(status: u16, body: &'static str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: Cow::Borrowed(body.as_bytes()),
        }
    }

    const fn reason(&self) -> &'static str {
        match self.status {
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            405 => "Method Not Allowed",
            431 => "Request Header Fields Too Large",
            _ => "Error",
        }
    }

    /// 序列化成完整的 HTTP 响应。
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
            self.status,
            self.reason(),
            self.content_type,
            self.body.len()
        );
        let mut out = head.into_bytes();
        out.extend_from_slice(&self.body);
        out
    }
}

/// 按扩展名猜内容类型。
#[must_use]
pub fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some(_) | None => "application/octet-stream",
    }
}

/// 在嵌入的文件里找静态资源。没有扩展名的路径回退到首页，供前端路由使用。
#[must_use]
pub fn static_asset(assets: &'static [(&'static str, &'static [u8])], path: &str) -> Response {
    let find = |wanted: &str| assets.iter().find(|(name, _)| *name == wanted).copied();
    let wanted = if path == "/" { "/index.html" } else { path };
    let hit = find(wanted).or_else(|| {
        let last = wanted.rsplit('/').next().unwrap_or(wanted);
        if last.contains('.') {
            None
        } else {
            find("/index.html")
        }
    });
    match hit {
        Some((name, body)) => Response {
            status: 200,
            content_type: content_type(name),
            body: Cow::Borrowed(body),
        },
        None => Response::text(404, "not found\n"),
    }
}

/// 接口层需要的数据来源。由节点实现；测试里用假实现。
pub trait Api {
    /// `/api/status` 的 JSON。
    fn status(&self) -> String;
    /// 为某区域选一个节点，返回它的 JSON；没有可用节点时返回 `None`。
    fn route(&self, region: &str) -> Option<String>;
    /// `/api/cluster`：全集群的节点。
    fn cluster(&self) -> String {
        "{\"nodes\":[]}".into()
    }
    /// `/api/usage`：某月（`None` 表示本月）的中继用量和计费。
    fn usage(&self, _month: Option<u32>) -> String {
        "{\"accounts\":[]}".into()
    }
    /// 管理操作。令牌不对返回 `Err((401, …))`；参数不对返回 `Err((400, …))`。
    fn admin(&self, _token: Option<&str>, _action: Admin) -> Result<String, (u16, String)> {
        Err((403, "这个节点不能做管理操作".into()))
    }
}

/// 管理操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admin {
    /// 加入一个集群节点（它的集群地址）。
    AddPeer(String),
    /// 签发一个会员令牌，每月中继额度（GB，0 表示不限）。
    IssueAccount(u64),
}

fn error_json(message: &str) -> String {
    format!("{{\"error\":{}}}", crate::json::string(message))
}

fn admin(api: &dyn Api, request: &Request, action: Result<Admin, &str>) -> Response {
    match action {
        Err(why) => Response::json(400, error_json(why)),
        Ok(action) => match api.admin(request.token.as_deref(), action) {
            Ok(body) => Response::json(200, body),
            Err((status, why)) => Response::json(status, error_json(&why)),
        },
    }
}

/// 处理一个请求。
#[must_use]
pub fn respond(
    request: &Request,
    api: &dyn Api,
    assets: &'static [(&'static str, &'static [u8])],
) -> Response {
    if request.method == "POST" {
        return match request.path.as_str() {
            "/api/peers" => {
                let addr = json_field(&request.body, "addr").filter(|a| !a.is_empty());
                admin(
                    api,
                    request,
                    addr.map(|a| Admin::AddPeer(a.to_owned()))
                        .ok_or("缺少 addr"),
                )
            }
            "/api/accounts" => {
                let quota = json_field(&request.body, "quota_gb").and_then(|q| q.parse().ok());
                admin(
                    api,
                    request,
                    quota.map(Admin::IssueAccount).ok_or("quota_gb 应为整数"),
                )
            }
            _ => Response::text(405, "method not allowed\n"),
        };
    }
    if request.method != "GET" {
        return Response::text(405, "method not allowed\n");
    }
    match request.path.as_str() {
        "/api/cluster" => Response::json(200, api.cluster()),
        "/api/usage" => {
            let month = query_param(&request.query, "month").and_then(|m| m.parse().ok());
            Response::json(200, api.usage(month))
        }
        "/healthz" => Response::text(200, "ok\n"),
        "/api/status" => Response::json(200, api.status()),
        "/api/route" => match query_param(&request.query, "region") {
            None | Some("") => {
                Response::json(400, "{\"error\":\"missing region parameter\"}".to_owned())
            }
            Some(region) => match api.route(region) {
                Some(node) => Response::json(200, format!("{{\"node\":{node}}}")),
                None => Response::json(404, "{\"error\":\"no healthy node\"}".to_owned()),
            },
        },
        path if path.starts_with("/api/") => {
            Response::json(404, "{\"error\":\"unknown endpoint\"}".to_owned())
        }
        path => static_asset(assets, path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static ASSETS: &[(&str, &[u8])] = &[
        ("/assets/app.js", b"console.log(1)"),
        ("/index.html", b"<html></html>"),
    ];

    struct Fake;
    impl Api for Fake {
        fn status(&self) -> String {
            "{\"ok\":true}".into()
        }
        fn route(&self, region: &str) -> Option<String> {
            (region == "us").then(|| "{\"id\":1}".into())
        }
    }

    fn get(target: &str) -> Response {
        let request = parse_request(&format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n")).unwrap();
        respond(&request, &Fake, ASSETS)
    }

    fn body(r: &Response) -> &str {
        std::str::from_utf8(&r.body).unwrap()
    }

    #[test]
    fn request_line_parsing() {
        let r = parse_request("GET /a/b?x=1&y=2 HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(
            (r.method.as_str(), r.path.as_str(), r.query.as_str()),
            ("GET", "/a/b", "x=1&y=2")
        );
        assert_eq!(parse_request("GET / HTTP/1.0").unwrap().query, "");
        for bad in [
            "",
            "GET",
            "GET /",
            "GET / HTTP/2",
            "GET nope HTTP/1.1",
            "GET / HTTP/1.1 extra",
        ] {
            assert_eq!(parse_request(bad), None, "{bad:?}");
        }
        assert_eq!(query_param("a=1&region=us&b", "region"), Some("us"));
        assert_eq!(query_param("a=1", "region"), None);
        assert_eq!(query_param("", "region"), None);
    }

    #[test]
    fn api_routes() {
        let r = get("/healthz");
        assert_eq!((r.status, body(&r)), (200, "ok\n"));
        let r = get("/api/status");
        assert_eq!((r.status, body(&r)), (200, "{\"ok\":true}"));
        assert!(r.content_type.starts_with("application/json"));
        let r = get("/api/route?region=us");
        assert_eq!((r.status, body(&r)), (200, "{\"node\":{\"id\":1}}"));
        assert_eq!(get("/api/route?region=mars").status, 404);
        assert_eq!(get("/api/route").status, 400);
        assert_eq!(get("/api/route?region=").status, 400);
        assert_eq!(get("/api/nope").status, 404);
    }

    #[test]
    fn static_files_and_spa_fallback() {
        let r = get("/");
        assert_eq!((r.status, body(&r)), (200, "<html></html>"));
        assert!(r.content_type.starts_with("text/html"));
        let r = get("/assets/app.js");
        assert_eq!(r.status, 200);
        assert!(r.content_type.starts_with("text/javascript"));
        // 前端路由：没有扩展名的未知路径给首页
        assert_eq!(body(&get("/nodes/3")), "<html></html>");
        // 有扩展名的未知路径是 404
        assert_eq!(get("/assets/missing.js").status, 404);
        // 没有首页时也不会崩
        assert_eq!(static_asset(&[], "/").status, 404);
        assert_eq!(static_asset(&[], "/x").status, 404);
    }

    #[test]
    fn only_get_is_allowed() {
        let request = parse_request("POST /api/status HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(respond(&request, &Fake, ASSETS).status, 405);
    }

    #[test]
    fn response_serialisation() {
        let bytes = get("/healthz").to_bytes();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Length: 3\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.ends_with("\r\n\r\nok\n"));
        for (status, reason) in [
            (400, "Bad Request"),
            (404, "Not Found"),
            (405, "Method Not Allowed"),
            (431, "Request Header Fields Too Large"),
            (500, "Error"),
        ] {
            let head = String::from_utf8(Response::text(status, "").to_bytes()).unwrap();
            assert!(head.starts_with(&format!("HTTP/1.1 {status} {reason}\r\n")));
        }
    }

    #[test]
    fn content_types() {
        for (path, expected) in [
            ("/a.html", "text/html"),
            ("/a.js", "text/javascript"),
            ("/a.css", "text/css"),
            ("/a.json", "application/json"),
            ("/a.js.map", "application/json"),
            ("/a.svg", "image/svg+xml"),
            ("/a.png", "image/png"),
            ("/a.ico", "image/x-icon"),
            ("/a.woff2", "font/woff2"),
            ("/a.bin", "application/octet-stream"),
            ("/noext", "application/octet-stream"),
        ] {
            assert!(content_type(path).starts_with(expected), "{path}");
        }
    }
}
