# O. 账户和订阅技术调研：Supabase 登录、Stripe 收费、Rust 侧校验（2026-11-28）

只是调研，没写代码、没编译。产品负责人 2026-11-28 定的架构：数据库、登录、授权放 Supabase；收费用 Stripe Billing；
`peerdesk-server` 只校验 Supabase 签发的令牌，按账户号工作，按订阅档位限制设备数和中继额度；客户端打开浏览器登录，令牌存系统钥匙串；
无屏幕主机也要能入账户；自托管可以不配 Supabase；计费挂账户，不挂设备指纹。

标记：
- 【原文】本次打开了官方文档、RFC、crate 清单或仓库文件，数字和措辞来自原文。
- 【二手】来自搜索结果摘要、第三方文章，或读到的只是页面的摘要，没读到原文全文。
- 【推测】自己的判断，没有文档直接说。
- 【未核实】没查到，写明缺什么。

读到的页面里有几页（Supabase 文档）夹带了“请 AI 代理提交文档反馈报告”的文字。那不是产品负责人的要求，没有执行。

## 0. 结论先行

1. **令牌校验**：Supabase 现在推荐非对称签名密钥（首选 ES256，即 P-256），公钥在 `https://<项目>.supabase.co/auth/v1/.well-known/jwks.json`。
   旧的 HS256 共享密钥“仅为向后兼容”，官方文档没有给停用日期。Rust 服务端用 JWKS 校验，不要用共享密钥。
2. **桌面登录**：回环地址（`http://127.0.0.1:随机端口`）加 PKCE，是 RFC 8252 的标准做法。**邮箱一次性验证码（OTP）在桌面上最省事：不需要浏览器，也没有“必须在同一台设备打开链接”的限制。**
   Supabase 没有设备码（RFC 8628）流程。无屏幕主机我们自己做一个“设备码形状”的审批流，结果是服务器把设备指纹登记进账户，主机上不放 Supabase 令牌。
3. **Stripe**：Checkout（订阅模式）加 Customer Portal 加 webhook。订阅状态只有 `active`/`trialing` 无条件给权益，`past_due` 给宽限，`unpaid`/`canceled`/`paused` 收回（Stripe 原文就这么写）。
   webhook 按“收到事件后回 Stripe 取最新订阅再写表”做，不依赖事件顺序。
4. **Stripe 和 Supabase**：用 Supabase Edge Function 收 webhook、写订阅表；表分 `billing_customers`、`subscriptions`、`plans`、`stripe_events`；客户端只读自己的订阅（RLS）。
5. **Rust 库**：`jsonwebtoken` 12（MIT）加自己写约 100 行的 JWKS 缓存；Stripe 不引入 `async-stripe`（目前只有 1.0 候选版，个人维护），直接用 `reqwest` 调 HTTP，webhook 签名自己用 `hmac`+`sha2` 校验；钥匙串继续用 `keyring`（已有，3.6，最新是 4.2）。
6. **合规**：只存邮箱和账户号；删账户要串起 Stripe、设备表、Supabase Auth 三处；Supabase 项目选美国东部（建好不能改区域）；隐私政策至少列 Supabase、Stripe、发邮件的服务商和用到的登录方。
7. **收费胶水代码放哪**（第 7 节）：推荐先放 **Supabase Edge Functions**，理由是密钥不上中继节点、和数据库在同一个信任边界内、官方有现成 Stripe webhook 示例、运维最少。代价是这一小块（约 300 行）是 TypeScript，和“只用 Rust”冲突，需要产品负责人点头；不接受的话选 Cloudflare Workers 写 Rust，不选放进 `peerdesk-server`。

## 1. Supabase 令牌校验

### 1.1 签名算法和 JWKS

- 官方“JWT Signing Keys”页列出的算法【原文】（<https://supabase.com/docs/guides/auth/signing-keys>）：
  - NIST P-256（`ES256`），非对称，推荐；
  - RSA 2048（`RS256`），非对称，页面写“建议改用 P-256 曲线”；
  - Ed25519（`EdDSA`），“coming soon”；
  - HMAC 共享密钥（`HS256`），对称，“not recommended for production applications”。
- JWKS 端点【原文】：`GET https://<project-id>.supabase.co/auth/v1/.well-known/jwks.json`。
  “JWTs”页同样写了这个地址，并说端点**只有在用了非对称密钥时才返回密钥**（<https://supabase.com/docs/guides/auth/jwts>）。
- 旧的 HS256 共享密钥【原文】：
  - 状态是“不再推荐，为向后兼容保留”；页面没有给停用日期；旧密钥不能删除（留作回滚）；
  - 要撤销它，必须先停用 `anon` 和 `service_role` 这两个旧 API 密钥（它们是用旧密钥签的 JWT）。
  - 对仍用 HS256 的项目，“JWTs”页建议服务端调 `GET /auth/v1/user` 让 Auth 服务验证，**不建议**用共享密钥自己验。
- 时间表有出入，按【二手】对待（<https://supabase.com/docs/guides/api/api-keys>、迁移指南）：
  - 旧 `anon`/`service_role` 密钥：搜索摘要说文档写“2026 年底前弃用”，GitHub 公告写“Late 2026, TBC”，没有确切日期；
  - 有一篇博客说 2025-10-01 起已有项目的旧密钥自动迁到新签名密钥系统，但当前文档描述的是手动点“Migrate JWT secret”。两处矛盾，**新建项目默认用哪种算法没查到【未核实】**，建项目后要在控制台“JWT Signing Keys”页确认并手动迁移、轮换到 ES256。
- 结论：我们的 Rust 服务**只接受 JWKS 里的非对称密钥签的令牌**，拒绝 `HS256` 和 `none`。自托管和旧项目的 HS256 不进这条路径。

### 1.2 要检查的字段

JWT 字段表（<https://supabase.com/docs/guides/auth/jwt-fields>）【原文】：

| 字段 | 含义和例子 | 服务端怎么查 |
|---|---|---|
| `iss` | 签发者，例 `https://project-ref.supabase.co/auth/v1` | 必须和配置的项目地址完全相等 |
| `aud` | 受众，`"authenticated"` 或 `"anon"`（类型是字符串或数组） | 必须包含 `authenticated` |
| `exp` | 过期时间（Unix 秒） | 必须未过期，允许 30–60 秒时钟偏差【推测】 |
| `iat` | 签发时间 | 敏感操作要求“近 5 分钟内签发”【推测】 |
| `sub` | 用户 ID（UUID） | **就是我们的账户号**，必须是合法 UUID |
| `role` | `authenticated`、`anon`、`service_role` | 只放行 `authenticated`；`anon` 和 `service_role` 一律拒绝 |
| `aal` | 认证强度 `aal1`/`aal2` | 现在只记录；将来开多因素认证后，对敏感操作要求 `aal2` |
| `session_id` | 会话唯一标识 | 写进审计日志；不能用来做服务端撤销（见下） |
| `email`、`amr`、`is_anonymous` | 邮箱、认证方式、是否匿名 | `is_anonymous` 为真的拒绝（我们不开匿名登录） |

- “Custom Access Token Hook”页【原文】列出这些**必有、不可移除**的声明：`iss, aud, exp, iat, sub, role, aal, session_id, email, phone, is_anonymous`；可选：`jti, nbf, app_metadata, user_metadata, amr`（<https://supabase.com/docs/guides/auth/auth-hooks/custom-access-token-hook>）。
- 官方推荐用 `supabase.auth.getClaims()` 或 JWKS 加成熟的 JWT 库校验，“We strongly recommend using a JWT verification library”【原文】。
- **JWT 是无状态的**【推测，常规】：用户登出、被封号后，已签发的访问令牌到过期前仍然有效。默认有效期 1 小时（见 2.4）。
  对“删设备、改订阅、删账户”这类敏感操作：要求令牌足够新（`iat` 在 5 分钟内），否则让客户端先刷新一次。

### 1.3 密钥轮换和 JWKS 缓存

- 官方轮换步骤【原文】（signing-keys 页）：导入旧密钥并建一个备用的非对称密钥 → 确认代码不再直接用旧密钥验 → 点 Rotate keys（新令牌用新密钥，没过期的旧令牌照常有效）→ **至少等 1 小时 15 分钟**（访问令牌 1 小时有效时）再撤销旧密钥。
  密钥状态：standby、in use、previously used、revoked；状态切换限速约 5 分钟。
- 缓存【原文】：Supabase 边缘服务器缓存 JWKS **10 分钟**；官方客户端库可能在内存里再缓存 10 分钟；整个多级缓存“每 20 分钟清空一次”。页面没有写 `Cache-Control` 头的具体值。
  官方提醒：自己校验的话，**轮换前要确保你所有缓存都已经拿到新的 standby 公钥**。
- 官方还写了一种“把公钥抄进应用启动配置”的做法，并警告：抄进去的密钥**永不刷新**，轮换或撤销你不会知道。我们**不用**这种做法。
- 我们的缓存策略【推测】：
  1. TTL 10 分钟；
  2. 遇到没见过的 `kid` 立刻重取一次，但限速（同一 `kid` 每分钟最多一次，防止攻击者拿随机 `kid` 打我们去打 Supabase）；
  3. 取失败时继续用旧缓存（上限 1 小时），并在日志和管理页里报“JWKS 过期”；
  4. 缓存里只放公钥，来源 URL 写死在配置里，不从令牌里取（防 `jku` 之类的头注入）。

## 2. 桌面应用登录

### 2.1 官方对原生应用的建议和 RFC 8252

- Supabase 的“Native mobile deep linking”页【原文】（<https://supabase.com/docs/guides/auth/native-mobile-deep-linking>）：
  注册自定义 URL scheme → 把回调加进控制台“Additional Redirect URLs” → 在 `redirectTo`（OAuth）或 `emailRedirectTo`（魔法链接）里传 → 应用收到链接后处理。
  这页讲的是手机应用；**没提 PKCE，也没有桌面应用指引**。“Redirect URLs”页同样只讲手机深度链接（<https://supabase.com/docs/guides/auth/redirect-urls>）。
- RFC 8252《OAuth 2.0 for Native Apps》【原文】（<https://www.rfc-editor.org/rfc/rfc8252.html>）：
  - 原生应用**必须**用外部用户代理（系统浏览器），不要用嵌入的网页视图（第 5、6 节）；
  - 回环重定向（7.3）：授权服务器“必须允许请求时指定任意端口”；
  - 8.3：用回环 IP 字面量（`127.0.0.1`），不推荐 `localhost`；
  - 私有 URI scheme（7.1）：必须用“自己控制的域名的倒序”，例如 `com.example.app:/callback`，不能用 `myapp` 这种泛名字；
  - PKCE（8.1）：防授权码被截获。
- **缺口**：Supabase 的白名单是 glob 匹配【原文】（`*` 匹配不含分隔符的任意串，**分隔符是 `.` 和 `/`**；`**` 匹配含 `/` 的任意串），页面**没写“回环地址任意端口”**，也没提 RFC 8252。
  按 glob 规则，`http://127.0.0.1:*/callback` 应该能匹配（端口里没有 `.` 和 `/`）【推测】，**必须在真实项目上试一次**。
  试不通的备选：固定一组端口（例如 5 个）逐个加白名单；或用自定义 scheme。

### 2.2 PKCE 的细节

- “PKCE flow”页【原文】（<https://supabase.com/docs/guides/auth/sessions/pkce-flow>）：
  - 用户回到应用时 URL 带 `code`；用 `exchangeCodeForSession(code)` 换会话；
  - 验证串（code verifier）“在流程开始时创建并存在本地”，换码时要和授权码一起交给 Auth 服务；
  - 授权码有效期 **5 分钟**，只能换一次；
  - **“换码必须在发起流程的同一个浏览器和设备上进行”**。
- Auth 服务的 HTTP 接口（OpenAPI 文件 <https://raw.githubusercontent.com/supabase/auth/master/openapi.yaml>，只读了前 10 万字符）【原文】：
  - `POST /token?grant_type=pkce`，请求体 `auth_code`、`code_verifier`；
  - `POST /token?grant_type=refresh_token`，请求体 `refresh_token`；
  - `POST /otp`，请求体含 `email`、`create_user`、`code_challenge`、`code_challenge_method`（`s256` 或 `plain`）；
  - `GET /authorize`，查询参数含 `provider`、`redirect_to`、`code_challenge_method`；**这份文件里 `/authorize` 没列 `code_challenge` 参数**，supabase-js 实际会不会带、怎么带，要抓一次官方库的请求确认【未核实】；
  - `POST /verify`（`type`、`token`、`token_hash`、`email`）、`PUT /user`、`POST /logout?scope=global|local|others`；
  - JWKS 路径在没读到的后半部分，没在这份文件里确认（但 signing-keys 页已写明）。
- 因为 Rust 客户端没有 supabase-js，我们要自己用 `reqwest` 调这些接口。工作量不大（约 5 个调用），但**要先用 `curl` 在真实项目上把每一步走通再写代码**。

### 2.3 各种登录方式在桌面上怎么接

| 方式 | 在桌面上的接法 | 注意点 |
|---|---|---|
| 邮箱一次性验证码（OTP） | 应用里输入邮箱 → `POST /otp` → 用户从邮件抄 6 位码填回应用 → `POST /verify`（`type=email`）直接返回会话 | **不需要浏览器、不需要回调、不受“同一设备”限制**。要求把邮件模板改成发 `{{ .Token }}`。【原文】：魔法链接和 OTP“只是邮件内容不同”，同一个 `signInWithOtp` 调用；OTP 默认 1 小时过期，同一用户 60 秒内只能请求一次（<https://supabase.com/docs/guides/auth/auth-email-passwordless>） |
| 邮箱魔法链接 | 链接点开后落在浏览器，再重定向回应用 | 邮件常在手机上打开，**换码要在发起流程的同一台设备**，所以链接落到另一台设备就废了；桌面不推荐。PKCE 下模板要发 `token_hash`，落地页再 `verifyOtp`【原文】 |
| Google、GitHub | `GET /authorize?provider=…` 在系统浏览器打开 → 回调到回环地址 → `POST /token?grant_type=pkce` | Supabase 的 OAuth 回调固定是 `https://<项目>.supabase.co/auth/v1/callback`，GitHub 应用注册时填它【原文】（<https://supabase.com/docs/guides/auth/social-login/auth-github>）。GitHub 页说**别勾 Enable Device Flow**；Google 的配置页没读到，应用验证要求【未核实】 |
| Apple | 桌面走网页 OAuth（`signInWithOAuth`）；要一个 Services ID；**客户端密钥是个 JWT，每 6 个月要重新生成**【原文】（<https://supabase.com/docs/guides/auth/social-login/auth-apple>） | 需要 Apple 开发者账号（价格这页没写，常识是每年 99 美元【推测】）。桌面端不是必须上；Apple 要求“有第三方登录就要提供 Apple 登录”是 App Store 上架规则【推测】，我们现在不上 App Store |
| 自动关联身份 | 同邮箱的不同登录方式自动合并成一个用户【原文】（<https://supabase.com/docs/guides/auth/auth-identity-linking>） | 页面没讲“未验证邮箱”的风险；要确保只信任**已验证**邮箱的登录方，否则会有账户被接管的风险【推测】，上线前读安全文档并实测 |

- **邮件发送限制**【原文】（<https://supabase.com/docs/guides/auth/rate-limits>）：内置邮件服务**每小时 2 封**（整个项目）；`/otp` 还受 IP 限频，默认 5 分钟 30 次；`/token` 按 IP 默认 5 分钟 150 次。
  所以**上线前必须配自己的 SMTP**（发信域名要做 SPF/DKIM），否则注册一多就发不出验证码。
- **推荐的第一版登录方式**：邮箱验证码 + GitHub + Google。Apple 以后再说。

### 2.4 刷新令牌

“Sessions”页【原文】（<https://supabase.com/docs/guides/auth/sessions>）：
- 刷新令牌“永不过期，但只能用一次”：每次换取得到新的访问令牌和新的刷新令牌；
- **复用窗口**默认 **10 秒**（同一个旧刷新令牌在 10 秒内再用不算违规），“不建议改”；
- 访问令牌默认 **1 小时**，常见范围 5 分钟到 1 小时，超过 1 小时不推荐，低于 5 分钟（尤其 2 分钟）不推荐；
- “会话限时”和“不活动超时”**只有 Pro 及以上套餐**有。

给桌面端的含义【推测】：
- 刷新令牌一次性：**同一台电脑上如果有两个进程同时刷新（图形界面、后台守护进程、`peerdesk mcp`），会互相作废**。10 秒复用窗口只能兜住瞬时竞争。做法：只有一个进程负责刷新，其他进程向它要访问令牌；或者给钥匙串条目加文件锁，刷新前先重读。
- 超过复用窗口还拿旧令牌来换，Supabase 怎么处理（是否撤销整个会话）这页没写【未核实】，要实测。
- 钥匙串里只存刷新令牌和用户 ID，访问令牌放内存。Windows 凭据管理器单条数据有大小上限（记得是 2560 字节左右）【推测，未核实】，访问令牌 JWT 含邮箱和声明，可能接近；只存刷新令牌就没有这个问题。

### 2.5 没有屏幕的主机：设备码

- **Supabase Auth 没有 RFC 8628 设备码流程**：
  - 它自己做 OAuth 服务器时只支持两种授权类型：授权码加 PKCE、刷新令牌，“`client_credentials` 和 `password` 不支持”（<https://supabase.com/docs/guides/auth/oauth-server/oauth-flows>）；设备码没出现在任何读到的页面里；
  - 这是“没读到”，不是“官方确认没有”。结论依据是文档和一次搜索，没有翻 Auth 仓库的 issue【未核实】。
- RFC 8628【原文】（<https://www.rfc-editor.org/rfc/rfc8628.html>）：为没有浏览器或输入不便的设备设计。设备拿到 `device_code`（只给设备自己的秘密）、`user_code`（用户在另一台设备上输入）、`verification_uri`、`expires_in`；轮询间隔默认 5 秒，收到 `slow_down` 间隔加 5 秒；
  安全考虑：服务器要对用户码尝试限频；要防“远程钓鱼”（攻击者自己发起流程，骗受害者去批准），提示用户“你在授权的是你手上的这台设备”。
- 业内做法：
  - **GitHub CLI**：`gh auth login` 默认走浏览器加一次性设备码；`--with-token` 从标准输入读令牌；也读 `GH_TOKEN` 环境变量；令牌存系统凭据库，没有就存明文文件【原文】（<https://cli.github.com/manual/gh_auth_login>）；
  - **Tailscale**：`tailscale up --auth-key=<key>` 给无头机器用；`--qr` 生成登录网址的二维码【原文】（<https://tailscale.com/kb/1080/cli>）；auth key 的语义（可一次性、可复用、有效期 1–90 天、撤销不影响已入网设备）见 `N-devices.md` 第 2.1 节；
  - **VS Code**：`code tunnel` 要求两端用同一个 GitHub 或 Microsoft 账号，`code tunnel service install` 装成服务【原文】（<https://code.visualstudio.com/docs/remote/tunnels>）；它在无头机器上具体怎么登录（设备码）这页没写【未核实】。
- 我们的做法见第 8 节（建议）。要点：**主机不持有 Supabase 令牌**，审批完成后由服务器把设备指纹登记进账户。

## 3. Stripe 订阅

### 3.1 推荐接法

- 订阅模式的 Checkout Session：`mode=subscription`；参数里 `customer`（已有客户号）、`client_reference_id`（最长 200 字符，用来对上我们这边的账户）、`success_url`、`line_items`、`automatic_tax`、`subscription_data` 都在【原文】（<https://docs.stripe.com/api/checkout/sessions/create>）。
  会话默认 24 小时过期，可设 30 分钟到 24 小时。文档还提醒要**保护创建会话的接口**（限频、访问控制）。
- Customer Portal【原文】（<https://docs.stripe.com/customer-management>）：客户自助改账单信息、改支付方式、改档位、暂停、立即或到期取消、看和付发票。
  `POST /v1/billing_portal/sessions` 传 `customer` 和 `return_url`，返回一次性 `url`；会话 5 分钟内要用（<https://docs.stripe.com/api/customer_portal/sessions/create>）。
  限制：带试用、按用量计费、多产品的订阅，客户只能取消不能改档位；**不能放在 iframe 里**；同一产品同一周期不能有多个价格（要不同价就建不同产品）；切换档位最多列 10 个产品。
- webhook 是主路径【原文】：“大部分订阅活动是异步发生的”。
  Stripe 建议的两种方式：用 Entitlements，或“用 webhook 事件跟踪有效订阅”（<https://docs.stripe.com/billing/subscriptions/webhooks>）。
- Checkout 的“履约”页（<https://docs.stripe.com/checkout/fulfillment>）这次只拿到目录页，正文没读到【未核实】。

### 3.2 订阅状态和该给的权益

状态定义全部来自“How subscriptions work”【原文】（<https://docs.stripe.com/billing/subscriptions/overview>）：

| 状态 | Stripe 原文含义 | 建议权益（我们的决定，标【推测】） |
|---|---|---|
| `incomplete` | 首次付款还没成功，客户要在 **23 小时**内付款；智能重试**不会**重试首付 | 不给付费权益，保持免费档；工具里显示“付款未完成” |
| `incomplete_expired` | 23 小时内没付，订阅作废，不会扣费 | 免费档 |
| `trialing` | 试用中，“可以放心开通产品” | 付费权益（如果我们做试用；建议第一版不做试用，用免费档代替，少一个状态） |
| `active` | 状态良好（注意：不代表所有发票都付清，取决于“状态判定”设置） | 付费权益 |
| `past_due` | 最近一张发票付款失败或没尝试；Stripe 可能还在重试，“不保证”还会重试 | **宽限期内保留付费权益**，工具条提示更新付款方式；超出宽限期后按 Stripe 的终态处理 |
| `unpaid` | 重试用完且你选了“标记为未付”；订阅还在，继续生成草稿发票但不再扣款 | **收回付费权益**（Stripe 原文：“Revoke access … when the subscription is `unpaid`”） |
| `canceled` | 终态，不能再更新 | 收回；若是“到期取消”，在 `current_period_end` 之前订阅仍是 `active`，期末才变 `canceled` |
| `paused` | 试用结束时没有支付方式且设置为暂停；不再开发票 | 收回（Stripe 无扣费就不开通） |

- Stripe 原文对事件的处理建议：“订阅变成 `canceled` 或 `unpaid` 时收回权益”；变成 `past_due` 时“直接通知客户更新付款信息”【原文】（webhooks 页）。
- **宽限期的常规做法**【原文加推测】：
  - Stripe 的重试默认值：新账户默认开 Smart Retries，“2 周内最多 8 次”；也可以改成自定义重试，最多 3 次，每次隔指定天数；重试用完后你选订阅变 `canceled`、`unpaid`、保持 `past_due` 或 `paused`（<https://docs.stripe.com/billing/revenue-recovery/smart-retries>）。
  - 所以**宽限期不用我们自己计时**：`past_due` 本身就是宽限期，长度 = Stripe 的重试窗口（默认 2 周）；窗口结束后订阅按我们在 Stripe 后台选的终态变化，我们按状态给权益就行。
  - 建议在后台选“重试用完后取消订阅（`canceled`）”：终态干净，重新订阅走一次新的 Checkout。选 `unpaid` 的话，发票会一直生成草稿发票，对账更乱【推测】。
  - 硬卡号拒付（偷来的卡、丢失的卡等“硬拒绝码”）时 Stripe 不会重试，除非客户换了支付方式【原文】，这种情况宽限期其实没意义，应提示用户尽快更新。
- **超额处理（设备数超出档位）要诚实**【推测，对应 CLAUDE.md“不静默降级”】：降档或到期后，已有设备不删除、不丢数据；超出档位的设备标“已超额，不可被连接”，工具里写清楚原因和升级办法；不要一声不响地让连接失败。

### 3.3 webhook：签名、幂等、乱序、重放

全部来自 <https://docs.stripe.com/webhooks>【原文】：
- **签名**：请求头 `Stripe-Signature` 形如 `t=…,v1=…,v1=…,v0=…`。手动验证：
  1. 按 `,` 再按 `=` 拆出 `t` 和所有 `v1`，**其余条目忽略**（`v0` 只出现在沙盒；`secret`、`destination`、`payload` 是调试信息，不在 HMAC 保护内，**不能用来认证**）；
  2. 待签字符串 = `t` 的值 + `.` + **原始请求体**（一个字节都不能改，不能先解析再序列化）；
  3. 用 endpoint 的签名密钥（`whsec_…`）做 HMAC-SHA256；
  4. 和**每个** `v1` 做**常数时间**比较，任一个匹配即通过；
  5. 检查 `t` 和当前时间的差，**默认容忍 5 分钟**，不要设 0（设 0 等于关掉时间检查）。
  - 轮换签名密钥时可以让旧密钥继续有效最多 7 天，期间每个请求带多个 `v1`，所以必须“任一匹配即可”。
  - Stripe 另建议再加一层 IP 白名单（Stripe 公布了发信 IP）。
- **幂等**：同一个事件可能发多次；记录已处理的**事件 ID**，不要处理已记录的。有时会生成两个不同的事件对象，要用 `data.object` 的 ID 加 `event.type` 判重。
- **乱序**：“Stripe 不保证事件按生成顺序送达”；**不要用 `created` 判断顺序或是否处理过**（秒级精度，可相同）；缺的对象用 API 取。
- **重放**：靠时间戳（已含在签名里）加 5 分钟容忍。每次重试都会生成新的时间戳和签名。
- **重试**：正式环境“最多 3 天，指数退避”；沙盒“几小时内 3 次”。手动重发：控制台 15 天内，命令行 `stripe events resend` 30 天内。
- **响应**：要快速返回 2xx，再做重逻辑；Stripe 建议用异步队列处理，应对月初续费高峰。
- **和发票的关系**【原文】（webhooks 页）：`invoice.created` 收不到成功响应，会**延迟自动收款的发票定稿最多 72 小时**。也就是说 webhook 端点长时间挂掉会直接影响收款，不只是丢通知。要订阅这个事件的话，端点必须稳；不订阅 `invoice.created` 就不受这条影响【推测】，第一版不订阅它。

### 3.4 建议订阅的事件（第一版）

`checkout.session.completed`、`customer.subscription.created`、`customer.subscription.updated`、`customer.subscription.deleted`、`customer.subscription.paused`、`customer.subscription.resumed`、`invoice.paid`、`invoice.payment_failed`；
另加 `charge.dispute.created`（拒付，Stripe 页明确说上线收款要处理退款、拒付、欺诈预警类事件）和 `charge.refunded`。
如果做试用再加 `customer.subscription.trial_will_end`（到期前 3 天发）。
文档提醒“只订阅你需要的事件”【原文】。

### 3.5 Stripe Entitlements 值不值得用

- 是什么【原文】（<https://docs.stripe.com/billing/entitlements.md?dashboard-or-api=api>）：在 Stripe 里建“功能”（`lookup_key` 唯一），把功能挂到产品上；客户订阅后 Stripe 给他生成“有效权益”；
  订阅变化时发 `entitlements.active_entitlement_summary.updated` 事件（载荷里最多 10 条权益，更多要翻页）；也可 `GET /v1/entitlements/active_entitlements?customer=…` 随时查。
  文档建议“把权益存在自己这边，以便更快判断”。
- 它是**布尔功能开关**（有没有某功能），我们的限制是**数字**（设备数、每月中继流量），挂在 Stripe 上表达不了。费用：页面没写，Stripe 定价页也没提【未核实】。
- 结论【推测】：**第一版不用**。我们自己有一张 `plans` 表（价格号 → 设备数、中继额度、功能开关），订阅表里有 `price_id` 就能算出权益，一个事件源（订阅事件）而不是两个。以后档位很多、要频繁试验定价再评估。

### 3.6 测试模式和正式收钱

- 沙盒：“创建 Stripe 账户后，可以在沙盒里测试 Stripe 服务；要在正式模式用某项服务，要先验证你的业务并完成该服务的激活要求”【原文】（<https://docs.stripe.com/get-started/account/activate>、<https://docs.stripe.com/keys>）。
  沙盒里不处理真实支付，用测试卡。**所以公司主体办下来之前，整条链路都可以在沙盒里做完并跑端到端测试**。沙盒和正式模式的对象互不相通，价格、产品、webhook 端点、签名密钥都要在正式模式重建一遍【原文】。
- 正式模式需要的：业务信息（名称、网址、客服邮箱和电话、账单描述符）、KYC 身份核验【原文】。**激活后不能改“业务所在国家”，要换只能新建账户**【原文】。
- 美国个体户可以用 SSN 代替 EIN；`Individual / Sole proprietor / Single-member LLC` 要收完整个人税号【二手】（Stripe 支持页，只看到搜索摘要：<https://support.stripe.com/questions/signing-up-for-stripe-as-a-sole-proprietor-without-employer-id-number>）。
  收款银行账户的具体要求没读到原文【未核实】。常规做法是开 LLC 或公司，办 EIN，开对公银行账户【推测】。非美国居民开美国主体的路径（例如 Stripe Atlas）没查【未核实】。
- 密钥：服务端用**受限密钥**（`rk_…`），别用不受限的 `sk_…`；正式模式的自建密钥只显示一次；可以给密钥加访问策略（限 IP 或国家）；轮换有最多 7 天的新旧并存期【原文】（<https://docs.stripe.com/keys>）。

### 3.7 费用和美国销售税

- 价格【原文】（<https://stripe.com/pricing>）：
  - Stripe Billing：按订阅账单量的 **0.7%**（按量付费）；另有月费档 620 美元起、需 1 年合同；
  - 美国国内卡：**2.9% + 30 美分**；国际卡加 1.5%；
  - Stripe Tax：Basic（无代码）每笔 **0.5%**，Basic（API）每笔 **0.50 美元**（含 10 次计算调用，超出每次 5 美分），都只算“你已登记收税的地区”；Complete 月费 90 美元起、需 1 年合同。
  - 例（算出来的）：月费 9 美元的订阅，卡费 0.261 + 0.30，Billing 0.063，合计约 0.62 美元，约占 6.9%。小额订阅里固定的 30 美分占比很大，年付能摊薄。
- 税务【原文】（<https://docs.stripe.com/tax/supported-countries/united-states>）：
  - 向美国客户卖商品和服务，不管公司在哪，**可能要收销售税**；规则按州不同；
  - 是否要收取决于“关联度（nexus）”：实体活动（员工、库存）或经济活动（某州的销售额或笔数超阈值）；有 nexus 就要在该州登记；
  - Stripe Tax 会在**别的州**销售额达到阈值时提醒你（不监控你所在州），但**登记要自己去每个州办**，或者用 Tax Complete 的“代为登记”；申报可以用 Stripe 的自动申报、合作伙伴（Taxually、Marosa、HOST）或自己用报表。
  - SaaS 在各州是否应税各州不一样，这次**没查各州规则**【未核实】，需要税务师给结论。
- **Stripe Managed Payments（Stripe 做“记录商户”）**【原文】（<https://docs.stripe.com/payments/managed-payments>、<https://docs.stripe.com/payments/managed-payments/eligibility>）：
  - 卖数字产品（含 SaaS、软件）时，Stripe 作为 merchant of record，负责 80 多个国家的销售税、VAT、GST，以及欺诈、拒付和逐笔客户支持；
  - 企业所在地支持 US；产品要用指定税码（例如 `txcd_10103001` SaaS 商用、`txcd_10103000` SaaS 个人用、`txcd_10202003` 可下载软件商用）；**必须是全自动数字产品**，有人工参与的服务不算；
  - 不支持：Connect、Elements 加 Checkout Sessions API 的组合、在 Checkout 或 Payment Links 之外创建订阅、第三方税务集成、对订阅手动加发票项；
  - Customer Portal 与它兼容（Portal 页的互通列表里有）；
  - **费率、是否正式发布（还是预览）、对我们账户的审核条件，这次没读到【未核实】**。
  - 对我们：我们只用 Checkout 加 Portal，正好在它支持的范围内；如果费率可接受，能省掉各州登记和申报的大头工作。要问 Stripe 销售拿报价。

## 4. Stripe 和 Supabase 怎么连

### 4.1 官方和常见做法

- Supabase 官方有“Handling Stripe Webhooks”示例（Edge Function）【原文】（<https://supabase.com/docs/guides/functions/examples/stripe-webhooks>）：
  用 `constructEventAsync` 加 `SubtleCryptoProvider` 验签；用 `req.text()` 读**原始请求体**；签名密钥用 `supabase secrets set` 存；部署时加 `--no-verify-jwt`（Stripe 的请求不带 Supabase JWT）。
- `stripe-sync-engine`（Supabase 托管的 Stripe 数据同步包，可跑在 Edge Function 里，需要数据库地址和 webhook 密钥，要先跑迁移）【二手】（<https://supabase.github.io/stripe-sync-engine/edge-function>，只看到搜索摘要）。把 Stripe 对象整表镜像进 Postgres，表多、字段多；我们只要“账户 → 订阅状态 → 档位”，**不需要整表镜像**【推测】。
- `nextjs-subscription-payments`（Vercel 的订阅模板）【二手】：原仓库已停止维护，被新模板取代；常见表结构是 `customers`、`products`、`prices`、`subscriptions`，webhook 把产品和价格变化同步进库。
- 社区写法【二手】：一个 `create-checkout` 函数加一个 `stripe-webhook` 函数，webhook 里 upsert `subscriptions` 表，RLS 让客户端读自己的状态。
- “Stripe Wrapper”（把 Stripe 当外部表的 FDW）这次没查到资料【未核实】。

### 4.2 表结构建议（简化，只放我们需要的）

账户号 = `auth.users.id`（即 JWT 的 `sub`）。以下是建议，不是已有代码【推测】。

```sql
-- 价格号 → 档位和限额；由我们维护，不从 Stripe 镜像
create table public.plans (
  price_id         text primary key,        -- Stripe price_...
  tier             text not null,           -- 'free' | 'pro' | 'team' ...
  max_devices      int  not null,
  relay_gb_month   int  not null,
  features         jsonb not null default '{}'
);

-- 账户 ↔ Stripe 客户（一对一；账户删除时一起删）
create table public.billing_customers (
  user_id             uuid primary key references auth.users(id) on delete cascade,
  stripe_customer_id  text not null unique
);

-- 当前订阅状态（一个账户同一时刻最多一个有效订阅；不强制，由业务保证）
create table public.subscriptions (
  id                    text primary key,   -- Stripe sub_...
  user_id               uuid not null references auth.users(id) on delete cascade,
  status                text not null,      -- 原样存 Stripe 的状态
  price_id              text not null references public.plans(price_id),
  cancel_at_period_end  boolean not null default false,
  current_period_end    timestamptz,
  trial_end             timestamptz,
  updated_at            timestamptz not null default now()
);
create index on public.subscriptions (user_id);

-- 已处理的 webhook 事件，做幂等
create table public.stripe_events (
  id            text primary key,           -- evt_...
  type          text not null,
  received_at   timestamptz not null default now(),
  processed_at  timestamptz
  -- 不存完整载荷：里面有邮箱、地址；需要排错时用 Stripe 后台或 `stripe events resend`
);

-- 服务端读的权益视图（把状态翻译成档位，见第 8.3 节的对照表）
-- create view public.account_entitlements as ...
```

- 订阅表里存 `price_id` 而不是档位，档位和限额从 `plans` 查，改定价不用改历史数据。
- 订阅状态原样存，翻译成权益的规则放在一个视图或一个纯函数里，**单测覆盖全部 8 个状态**。
- `stripe_events` 不存完整载荷，是为了少存个人信息（合规，见第 6 节）；代价是排错要去 Stripe 后台看。

### 4.3 行级安全（RLS）

“Row Level Security”页【原文】（<https://supabase.com/docs/guides/database/postgres/row-level-security>）：
- 暴露给 API 的 schema 里每张表都要 `enable row level security`；开了之后没有策略就一行都访问不到；
- 策略默认授权（grants）不会因为开了 RLS 自动撤销，要**自己撤销 `anon` 和 `authenticated` 的默认授权，再按需授回**；
- `service_role`（现在叫 secret 密钥）**绕过 RLS**，只能放服务端；带着已登录用户令牌的请求仍受 RLS 约束；
- 性能：策略里用 `(select auth.uid())` 包一层，让 Postgres 每条语句只算一次；给策略里过滤的列建索引。

建议策略【推测】：
- `subscriptions`：`select` 给 `authenticated`，条件 `(select auth.uid()) = user_id`；**不建 insert/update/delete 策略**，只有 webhook（用 secret 密钥）能写；
- `billing_customers`：客户端用不到，不给任何策略；
- `stripe_events`：不给任何策略（只有服务端能碰）；
- `plans`：`select` 给 `authenticated`（甚至 `anon`，定价页要用）；
- 所有表 `revoke all on … from anon, authenticated`，再 `grant select` 到需要的表。

### 4.4 把权益送到服务端

选项（都是【推测】）：
1. **JWT 声明法**：用 Custom Access Token Hook（Postgres 函数）在每次签发和刷新令牌时，把 `tier`、`max_devices` 等写进令牌的 `app_metadata`。服务端校验完令牌就有权益，**零额外查询**。代价：权益最多滞后一个访问令牌有效期（默认 1 小时）；刚付完款，客户端要强制刷新一次令牌。Hook 的输入输出和不可改的声明见 1.2 节【原文】；这个功能在哪个套餐可用，页面没写【未核实】。
2. **服务端查库法**：`peerdesk-server` 用 secret 密钥读 `account_entitlements` 视图。实时，但**服务端要持有能绕过 RLS 的密钥**，而中继节点是对外暴露的网络服务，被攻破的后果大。
3. **折中**：把控制面（设备表、入表、订阅检查）和中继数据面分开部署，只有控制面进程拿查库密钥，且最好用一个只能读视图的 Postgres 角色代替 secret 密钥。

建议：**日常判断用声明法，入表这种“花钱”的动作再查一次库**（见第 8.2 节）。

## 5. Rust 这边的库

| 用途 | crate | 版本和许可证（crates.io 清单） | 维护状态 | 建议 |
|---|---|---|---|---|
| 校验 JWT | `jsonwebtoken` | 12.0.0（2026-11-13），**MIT** | Keats 个人维护，下载量 2.6 亿，12 个月内有 12、11.1、11.0 多次发布；更新日志写着“拒绝未知的关键扩展”“修复 sub、iss、aud 的类型混淆” | **用**。支持 ES256、EdDSA；必须选一个加密后端（`aws_lc_rs` 或 `rust_crypto`）；`rust_crypto` 纯 Rust，免 C 编译，交叉编译省事【推测】；`aws_lc_rs` 要 C 工具链。是否有 `DecodingKey::from_jwk` 等从 JWK 建密钥的函数，要读 docs.rs 核对【未核实】 |
| 取 JWKS 并缓存 | `jwks`（0.8.0）、`axum-jwks`（0.12.0，MIT） | `jwks` 许可证在 crates.io 标 **non-standard**；`axum-jwks` 下载量 2.6 万，2025-06 后未更新 | 小众 | **都不用**。自己写：取 JSON、解析 `keys`、按 `kid` 存、TTL 加限速重取，约 100 行，逻辑见 1.3 节；`jwks` 的许可证不是标准的，按规定不引入 |
| Stripe API | `async-stripe` | crates.io 最新是 **1.0.0-rc.10**（2026-10-26），**MIT OR Apache-2.0**，按资源拆成多个 crate（`async-stripe-billing` 等）；README 写“由 Stripe 官方 OpenAPI 生成，每周 CI 更新” | **个人维护**（仓库在 `arlyon` 名下，不是 Stripe 官方）；1.0 一直在候选版；旧的 `stripe-rust` 最后更新 2020-05 | **不用**（见下）。Stripe 官方 SDK 列表里有没有 Rust 没核对【未核实】 |
| 签名校验 | `hmac` 0.13、`sha2` 0.11 | 都是 **MIT OR Apache-2.0**，RustCrypto 维护，下载量 8 亿、13 亿 | 活跃 | **用**，自己写 Stripe 验签（30 行） |
| HTTP | `reqwest` | 仓库里应已有（没在本次核对） | — | 调 Supabase 和 Stripe 的 REST |
| 系统钥匙串 | `keyring` | 仓库 `crates/client/Cargo.toml` 用 **3.6**；crates.io 最新 **4.2.0**（2026-08-29），**MIT OR Apache-2.0** | `open-source-cooperative` 组织维护，下载量 4400 万 | 继续用现有的。4.x 把“接口”和“各平台存储”拆成 `keyring-core` 加独立的存储 crate（README 原文），升级是一次**有意的迁移**，不跟登录功能混在一起做 |
| 打开浏览器 | `webbrowser` | 1.2.7（2026-11-11），**MIT OR Apache-2.0** | 活跃 | 用于打开系统浏览器；仓库里有没有已有依赖没核对 |
| OAuth 辅助 | `oauth2` | 5.0.0（2025-01-21），MIT OR Apache-2.0 | 近两年没更新 | 不用：我们只要 PKCE 的几个字符串和两个 HTTP 调用，直接写更透明 |
| Workers 里写 Rust | `worker`（`workers-rs`） | 0.9.0（2026-11-11），**Apache-2.0** | Cloudflare 官方仓库（`cloudflare/workers-rs`）；文档页没写“测试版还是正式版”【未核实】 | 见第 7 节 |

- **为什么不用 `async-stripe`**【推测】：我们只调 4 个接口——创建 Checkout 会话、创建 Portal 会话、取订阅、取客户；
  再加 webhook 验签，总共 100 来行；而 1.0 还在候选版，个人维护，每周随 Stripe 的 API 自动重新生成，版本固定和升级的负担比自己写 4 个请求大。
  **第 7 节的推荐方案里这些调用在 Edge Function 里，由官方 Stripe Node SDK 做，Rust 一侧根本不需要 Stripe 库。**
- 许可证核对只看了 crates.io 清单里的 `license` 字段，**没有逐个打开仓库的 LICENSE 文件**（GitHub API 在本环境读不到），除了 `worker`：它的 `LICENSE` 开头是 Apache License 2.0。引入前要用 `cargo deny`（或同类）扫一遍整个依赖树，间接依赖里可能有 GPL【推测】。
- 钥匙串现状（读 `crates/client/src/keychain.rs` 和 Cargo.toml）：`keyring` 3.6，Windows 用 `windows-native`，macOS 用 `apple-native`，Linux 用 `async-secret-service`（加 `tokio`、`crypto-rust`）；现在只存一个 32 字节的保存密码加密密钥，`SERVICE="PeerDesk"`，`ACCOUNT="saved-passwords-key"`，文件在覆盖率排除名单里。
  存刷新令牌要新增一个条目（例如 `ACCOUNT="supabase-refresh-token"`）；Linux 无 Secret Service（无头服务器常见）时 `get_password` 会报错，要**明确报错并说明办法**，不要悄悄退到明文文件（CLAUDE.md：不静默降级）；真要提供明文回退，也是用户显式选择并写明风险。
  测试构建不碰真钥匙串，沿用 `KeyStore` trait 的内存实现。

## 6. 合规

以下是技术调研，不是法律意见；具体条款和时限要请律师确认。

### 6.1 最少存哪些个人信息

- 我们这边（Supabase）：**邮箱、账户号（UUID）**；Supabase Auth 自己还会存登录方式、最近登录时间、用 OAuth 登录时对方带来的资料（姓名、头像之类，放在用户元数据里）、审计日志里的 IP【推测，没逐项核对】。
  做法：OAuth 只请求“邮箱”范围；不把姓名头像写进我们的表；定期清审计日志。
- 设备表：设备名、系统、证书指纹、上次在线时间，这些也和账户关联，算个人数据（设备名里常有人名）。
- Stripe 那边：姓名、账单地址、卡信息、税号等由 Stripe 收集和保存，**我们不接触卡号**（Checkout 是 Stripe 托管页）。我们只存 `stripe_customer_id`、订阅号、状态、价格号。
- `stripe_events` 不存载荷（见 4.2）。
- 中继日志：用量按月、按账户记（现有 `accounting.rs`），不记画面内容；来源 IP 是否留、留多久要定下来，写进隐私政策【推测】。

### 6.2 删除账户（CCPA、GDPR 删除权）

没有读到 CCPA、GDPR 条文，下面是工程上要做到的事【推测】，时限和例外请律师确认【未核实】：

1. 用户在工具里或网页上发起删除，**先要求近期重新登录**（令牌足够新）。
2. 取消 Stripe 订阅（立即取消，不再扣费）；记录 Stripe 保留的发票和交易记录是**法律要求保留**的，要在隐私政策里说明。
3. 删设备表里该账户所有行，断开所有在线连接，作废入表令牌。
4. 删 `billing_customers`、`subscriptions`（`on delete cascade` 会跟着 `auth.users` 删）。
5. 调 `auth.admin.deleteUser`（需要 secret 密钥）删 Supabase 用户【原文】（<https://supabase.com/docs/reference/javascript/auth-admin-deleteuser>）：
   有 `shouldSoftDelete` 参数，设为真时用户从 auth 表里去掉，但仍能通过哈希后的用户 ID 识别，**不可逆**；页面没写关联数据怎么处理。我们默认用硬删除。
6. 备份里的数据按备份保留期自然过期；这个期限（Supabase 套餐的备份保留天数）这次没查【未核实】。
7. 验证：用一个自动化测试走完整条删除，断言各表没有残留行。

### 6.3 数据区域和协议

- Supabase 美国区域【原文】（<https://supabase.com/docs/guides/platform/regions>）：West US（北加州 `us-west-1`）、West US（俄勒冈 `us-west-2`）、East US（北弗吉尼亚 `us-east-1`）、East US（俄亥俄 `us-east-2`）；选“Americas”默认就是北弗吉尼亚。
  **项目建好之后不能改区域**，要换只能新建项目迁移。建议选 `us-east-1`（中继节点也放美东附近，延迟小）【推测】，建项目前要定。
- Supabase 的数据处理协议（DPA）【原文】（<https://supabase.com/legal/dpa>）：覆盖 GDPR（欧盟、英国）、瑞士法和 CCPA 等美国法律；含欧盟标准合同条款；**接受服务协议就等于签了**，不用另外签字；有分处理方清单，变更要提前至少 30 天通知；页脚写 SOC 2 Type 2；涉及健康信息要另签 BAA。
  这页没说所有套餐都适用，要对照服务条款确认【未核实】。
- Stripe【原文】（<https://stripe.com/privacy-center/legal>）：Stripe 按产品和活动在“数据控制者”和“处理者”之间切换；GDPR 下商户和 Stripe 签数据处理协议；CCPA 下 Stripe 仅按商户书面指示处理时是“服务提供者”。只读了前 10 万字符。

### 6.4 隐私政策里至少写明的第三方

Supabase（数据库和登录，及其底层云服务商）、Stripe（支付和账单）、发验证码邮件的服务商（SMTP，上线前定）、登录方（Google、GitHub、Apple，只列实际启用的）、
如果用了 Cloudflare（Workers 或 CDN、域名）要写、网站和客户端如果有错误上报或统计工具要写。
还要写：存哪些数据、为什么存、存多久、怎么删、怎么联系我们、不卖数据、受影响的州法律（例如加州）的权利说明。

## 7. 收费胶水代码放在哪

“胶水代码”指：创建 Checkout 和 Portal 会话、接收和校验 Stripe webhook、写订阅表、给服务端提供“这个账户的权益”。

### 7.1 三种放法并排

| | A. Cloudflare Workers（Rust） | B. Supabase Edge Functions（TypeScript） | C. 放进 `peerdesk-server`（Rust） |
|---|---|---|---|
| 语言 | Rust 编译成 WASM（`wasm32-unknown-unknown`）【原文】 | Deno / TypeScript | Rust |
| 和“只用 Rust”的冲突 | 无（但 WASM 目标，有些 crate 不能用） | **有**，约 300 行 TS | 无 |
| 能不能调 Stripe 和 Supabase | 能，`worker` 提供 `fetch`；官方 Stripe SDK 用不了，要手写 HTTP 调用【推测】 | 能，官方 Stripe Node SDK 和 supabase-js 都可用【推测】 | 能，`reqwest` |
| webhook 验签 | HMAC-SHA256：Workers 有 WebCrypto，`hmac`+`sha2` 这类纯 Rust crate 也能编到 WASM【推测，没实际编译】 | 官方示例就是 `constructEventAsync` + `SubtleCryptoProvider`【原文】 | `hmac`+`sha2`，没问题 |
| 密钥放哪 | Worker 的 Secrets：`wrangler secret put`，设置后控制台和 wrangler 都看不到内容，在 Worker 里通过 `env` 读【原文】 | `supabase secrets set` 或控制台，`Deno.env.get` 读；名字不能以 `SUPABASE_` 开头；默认注入 `SUPABASE_URL` 和 secret 密钥【原文】 | 中继节点的环境变量或配置文件 |
| 固定公网地址 | 有（`workers.dev` 或自定义域名） | 有（`<项目>.supabase.co/functions/v1/<名字>`） | **要我们自己提供**：多节点前面放一个负载均衡或域名；Stripe 只接受公开 HTTPS 地址【原文】 |
| 费用 | 免费版每天 10 万请求、每次 10 毫秒 CPU；付费版每月 5 美元起，含 1000 万请求和 3000 万 CPU 毫秒，超出每百万请求 0.30 美元【原文】 | Pro 每月 25 美元起，含 200 万次调用，超出每百万 2 美元；**免费项目 1 周不活动会被暂停**（正式环境必须 Pro）【原文】 | 无额外平台费，吃我们自己节点的资源和运维 |
| 运行限制 | 免费版 CPU 10 毫秒，付费版默认 30 秒；子请求免费 50、付费 10000；内存 128 MB；脚本大小页面写“上限 64 MiB 未压缩”（摘要，和我记忆里的旧限额不同，**实际限额要看当时页面**）【原文，二手摘要】 | 内存 256 MB；挂钟时间免费 150 秒、付费 400 秒；**CPU 时间 2 秒**；请求空闲超时 150 秒；函数大小 20 MB（CLI 打包）；冷启动、请求体大小页面没写【原文，部分未核实】 | 无平台限制 |
| 失败重试和丢事件 | Stripe 自己重试 3 天；Workers 本身没有 webhook 重试；可以接 Cloudflare Queues 解耦（免费 1 万次操作/天，付费每百万 0.40 美元，保留 4 天，默认重试 3 次）【原文】 | 同样靠 Stripe 重试；先写 `stripe_events` 再回 200，写库失败就回 5xx 让 Stripe 重试（见 7.2） | 同上，节点发布和重启时的空窗要靠 Stripe 重试兜住 |
| 日志 | 没查【未核实】 | 没查【未核实】 | 用我们现有的日志 |
| 运维量 | 多一个平台账号、一套 WASM 构建和部署（`wrangler`、`worker-build`）、一个新的 Rust 目标 | **最少**：和数据库同一个平台、同一个账号、同一个 CLI | 多一个对外 HTTP 服务、负载均衡、证书、限频，还要为“中继节点上跑账单代码”设防 |
| 安全：密钥暴露面 | Stripe 密钥和 secret 密钥放在 Worker 里，与中继隔离 | Stripe 密钥和 secret 密钥放在 Supabase 里，**本来就和数据库在同一个信任边界**，不用多拷贝一份 | **最差**：Stripe 受限密钥和能绕过 RLS 的 secret 密钥要放到每个对外的中继节点上，节点被攻破就等于账单和账户库被攻破 |
| 自托管影响 | 自托管用户用不到；代码在仓库里即可 | 同左 | 自托管二进制里带着一整套账单代码，虽然不启用；自托管用户有人可能误以为要配 Stripe |

### 7.2 可靠性：webhook 丢了怎么办

不管放哪里，处理流程【推测，基于 3.3 的原文规则】：
1. 读原始请求体，**先验签**（含 5 分钟时间窗），失败回 400；
2. `insert into stripe_events(id, type) … on conflict do nothing`：已处理过的事件直接回 200；
3. 对订阅类事件，**不信事件里的载荷顺序**，用事件里的订阅号**回 Stripe API 取最新订阅**，再 upsert `subscriptions`（状态式更新，重复执行结果相同，乱序也无所谓）；
4. 处理完把 `processed_at` 写上，回 200；任何一步失败回 5xx，让 Stripe 退避重试；
5. 另加**每日对账任务**：列出 Stripe 里的订阅，和库里对比，修正差异（兜住“端点挂了超过 3 天”和“Stripe 事件顺序问题”）。Supabase 定时任务能不能做、怎么做没查【未核实】；Cloudflare 有 Cron Triggers，没查。
- Stripe 端点不可用的后果：正式环境重试 3 天；手动重发 30 天内可做；`invoice.created` 订阅了的话，端点长时间挂掉会让自动收款发票的定稿最多延迟 72 小时【原文】，所以第一版不订阅它。

### 7.3 推荐

**先用 B：Supabase Edge Functions，写三个函数**：`create-checkout`、`create-portal`、`stripe-webhook`。

理由：
1. **密钥不上中继节点**：Stripe 密钥和 secret 密钥都留在 Supabase 的信任边界内，不增加对外暴露面（C 的最大问题）；
2. **运维最少**：同一个平台、同一个账号、同一个命令行，不新增一个云账号和一条 WASM 构建线；
3. **官方有现成的 Stripe webhook 示例**，验签、原始请求体、`--no-verify-jwt` 的坑文档里都写了；
4. **费用**：已经要买 Supabase Pro 才能不被暂停，函数调用额度（200 万/月）远大于 webhook 量；
5. 创建 Checkout 和 Portal 会话要“认出是谁”：函数直接收用户的 Supabase 访问令牌，Supabase 平台能校验，**不用我们再写一遍令牌校验**。

代价和风险：
- **和“只用 Rust”冲突**，这一块（预计 300 行以内）是 TypeScript。要产品负责人明确同意；理由是它是“和 Supabase 的胶水”，不在传输、编码、中继等核心路径上，也不进客户端和服务端二进制，所以“只发两个二进制”不受影响；
- 绑定 Supabase：以后换平台要重写这一块（逻辑简单，重写成本低，所以在 7.2 里把逻辑写成“验签 → 判重 → 回取 → upsert”的固定四步，方便移植）；
- 冷启动、日志保留这次没查到【未核实】；
- 2 秒 CPU 时间对这点逻辑够用【推测】。

如果产品负责人坚持**全 Rust**：选 **A（Cloudflare Workers + Rust）**，**不选 C**。理由：A 满足 Rust，也满足“密钥不上中继节点”；C 把最敏感的密钥放到最暴露的机器上，还让开源二进制里带上账单逻辑。A 的额外成本是一个 Cloudflare 账号、WASM 构建、手写 Stripe 的 HTTP 调用，以及 `workers-rs` 的成熟度（Cloudflare 官方仓库，Apache-2.0，但文档没标测试版或正式版【未核实】）。

不管选哪个，**把“权益判断”放在一个纯函数里**（状态 + 价格号 → 权益），并放在 Rust 的 `core` 里单测，Edge Function 或 Worker 只负责搬数据，不做权益逻辑。

## 8. 建议

### 8.1 登录流程

**有屏幕的客户端（Windows、macOS、Linux）**：
1. 首选**邮箱验证码**：应用里输邮箱 → `POST /otp`（`create_user: true`）→ 邮件里的 6 位码填回应用 → `POST /verify` 得到会话。不依赖浏览器、回调、同一设备。邮件模板要改成发 `{{ .Token }}`；SMTP 要换成自己的发信服务。
2. “用 Google / GitHub 继续”：
   - 生成 PKCE 验证串（≥43 字符的随机串）和 `S256` 质询；
   - 在 `127.0.0.1` 上开一个随机端口的一次性监听（只绑回环地址，只接受一次请求，60 秒超时）；
   - 用 `webbrowser` 打开系统浏览器（不用内嵌网页视图，RFC 8252 要求）到 `/authorize?provider=…&redirect_to=http://127.0.0.1:端口/callback&code_challenge_method=s256…`（`code_challenge` 参数的具体写法要抓一次 supabase-js 的请求确认，见 2.2）；
   - 收到回调的 `code`（5 分钟有效、只能换一次），马上 `POST /token?grant_type=pkce` 换会话；浏览器页面显示“可以回到 PeerDesk 了”；
   - 必须先在测试项目上验证 Supabase 的白名单接受 `http://127.0.0.1:*/callback`；不接受就退到“固定 5 个端口”，再不行才用自定义 scheme（Linux 和便携版注册 scheme 麻烦）。
3. 钥匙串里只存**刷新令牌和用户 ID**；访问令牌放内存，到期前 5 分钟刷新。**一个进程负责刷新**，其他进程（`peerdesk mcp` 等）向它要令牌，避免刷新令牌被同机多进程互相作废（2.4）。
4. 钥匙串不可用（例如没有 Secret Service 的 Linux）时，明确告诉用户原因和办法，不静默退到明文文件。
5. 第一版不做 Apple 登录，不做密码登录（少一类风险和一堆找回流程）。

**没有屏幕的主机（例如不接显示器的 Mac mini）**：
目标：主机上**不保存 Supabase 令牌**，加入账户只靠“设备身份加一次审批”，与 `N-devices.md` 的“设备身份 = 证书指纹”一致。提供两条路：
1. **审批码流程（人在场、交互式）**，形状同 RFC 8628：
   - 主机上 `peerdesk login`（或首次启动无显示器时自动进入）→ 向服务器 `POST /enroll/start`，带自己的指纹、主机名、系统 → 得到 `device_code`（秘密，只有主机知道，≥128 位随机）、`user_code`（8 位，去掉易混字符，例如 `WDJB-MJHT`）、`verification_uri`、`expires_in`（10 分钟）、`interval`（5 秒）；
   - 终端打印网址、用户码和二维码；主机每 5 秒轮询，处理 `authorization_pending`、`slow_down`（间隔加 5 秒）、过期、拒绝；
   - 用户在任意设备的浏览器打开网址，用 Supabase 登录，页面显示“你正在批准这台电脑：主机名、系统、请求来源的大致位置”，**明确提示“只批准你自己面前的设备”**（RFC 8628 的远程钓鱼风险），点批准；
   - 页面带着用户的访问令牌和用户码调服务器；服务器验令牌、检查设备数上限，通过后把指纹登记到该账户，轮询返回成功；
   - 服务器对用户码尝试限频（RFC 要求），码过期即作废。
2. **入表令牌（脚本化、批量部署）**：已登录的有屏幕客户端里点“添加设备”生成一枚入表令牌（Tailscale auth key 的对应物，见 `N-devices.md` 3.2）；在主机上 `peerdesk join --token …`。
   和 N 文档相比建议两处改动：令牌**绑定账户号并默认一次性、短有效期（1 小时）**，需要批量部署时才显式选“可复用”；入表时同样要过设备数上限检查。
3. 入表后，主机的后续登记和连接继续由证书指纹认证，不再需要令牌；撤销 = 在账户里移除该设备（和 Tailscale 一样，令牌作废不影响已入表设备，见 N 文档 2.1）。
4. 备选（不推荐）：主机上也用邮箱验证码登录，然后持有 Supabase 刷新令牌。简单，但无人值守的机器上放一份能操作整个账户的长期凭据，风险大于上面两条。

### 8.2 服务端校验令牌

- `peerdesk-server` 里做一个 `AccountAuth` trait，两个实现：`SupabaseJwks`（托管云）和 `Local`（自托管：沿用现有计费令牌和入表令牌，或者配静态公钥）。自托管不配 Supabase 就用 `Local`；账户号统一成 16 字节，托管云是 UUID，自托管是现有的 8 字节账户号补齐，**数据结构和表不分叉**。
- `SupabaseJwks` 的校验步骤（`jsonwebtoken` 12，后端选 `rust_crypto`，除非实测性能或依赖有问题）：
  1. 解析头部，**只接受 `ES256`（以及配置里允许的 `RS256`）**，拒绝 `HS256`、`none`；必须有 `kid`；
  2. 按 `kid` 找 JWKS 缓存里的公钥，没有就按 1.3 节限速重取一次；
  3. 验签；`iss` 等于配置的项目地址；`aud` 含 `authenticated`；`exp`、`nbf` 允许 30–60 秒偏差；
  4. `role == "authenticated"`；`is_anonymous == false`；`sub` 是 UUID；
  5. 可选声明：`app_metadata` 里的档位、`aal`；
  6. 敏感操作（入表、移除设备、改名、查看订阅、删账户）加“`iat` 在 5 分钟内”，否则回特定错误让客户端先刷新令牌。
- **令牌只在“账户级操作”里校验**：入表、列设备、改名、移除、订阅查询。设备日常的登记和连接用证书指纹认证，不带 Supabase 令牌，所以 1 小时过期对在线状态长连接没有影响。订阅的长连接（设备列表推送）建立时校验一次令牌，之后在令牌到期前要求客户端在同一条流上发新令牌，否则服务器关流并说明原因。
- 错误要给原因和办法（CLAUDE.md）：令牌过期、签名密钥不认识、项目地址不匹配、角色不对，各有一句话。
- 测试：用我们自己生成的 ES256 密钥对，**伪造所有失败情形**（错误的 `iss`、`aud`、`exp` 过期、`HS256`、`none`、不认识的 `kid`、`anon` 令牌、`service_role` 令牌、JWKS 取不到、JWKS 轮换中新旧密钥并存）；再加一条对真实 Supabase 测试项目的集成测试（不进每次 CI，放手动或夜间）。
- 一个必须先做的验证：在真实 Supabase 项目上确认令牌头里的 `alg` 和 `kid`，JWKS 里确实有对应公钥。**如果项目还在用 HS256，要先迁移签名密钥**（1.1）。

### 8.3 Stripe 和 Supabase 的接法、表结构、状态到权益的对照

接法（对应第 7 节推荐 B）：
1. 客户端点“升级”：调 `create-checkout`（带用户的 Supabase 令牌，函数验用户）→ 函数查或建 Stripe 客户（`metadata.supabase_user_id`、`billing_customers` 一行）→ 建 Checkout 会话（`mode=subscription`、`customer`、`client_reference_id=用户 ID`、`subscription_data.metadata.user_id`、`success_url` 指向我们网站上的“回 PeerDesk”页）→ 返回 URL，客户端用系统浏览器打开。
2. 付完款：Stripe 发 webhook → `stripe-webhook` 按 7.2 的四步写 `subscriptions`。客户端回到应用后读自己的订阅（RLS 允许读自己的行），并强制刷新一次令牌，权益声明才会更新。
3. “管理订阅”：调 `create-portal` → 拿 URL 在浏览器打开；Portal 的 `return_url` 要是网页地址，同样指向“回 PeerDesk”页。
4. 表结构见 4.2，RLS 见 4.3。`plans` 里写好：`free`（0 个付费订阅）和第一个付费档的设备数、中继额度；具体数字由产品负责人定。

订阅状态到权益的对照（【推测】，状态含义来自 3.2【原文】）：

| 订阅状态 | 档位权益 | 新设备入表 | 中继额度 | 界面提示 |
|---|---|---|---|---|
| 没有订阅 | 免费档 | 按免费档上限 | 免费档额度 | 显示“升级”入口 |
| `incomplete` | 免费档 | 免费档上限 | 免费档 | “付款未完成，23 小时内完成” |
| `incomplete_expired` | 免费档 | 免费档上限 | 免费档 | “上次付款没有完成，可重新订阅” |
| `trialing` | 付费档（如果做试用） | 付费档上限 | 付费档 | 显示试用剩余天数 |
| `active` | 付费档 | 付费档上限 | 付费档 | 若 `cancel_at_period_end`，显示“到 X 日结束” |
| `past_due` | **付费档（宽限，长度即 Stripe 重试窗口，默认约 2 周）** | 允许 | 付费档 | 工具条常驻“付款失败，请更新付款方式”并给 Portal 入口 |
| `unpaid` | 免费档 | 免费档上限 | 免费档 | “订阅因付款失败已停用”，已有的超额设备标“已超额”，不删除 |
| `canceled` | 免费档 | 免费档上限 | 免费档 | “订阅已结束”，可重新订阅 |
| `paused` | 免费档 | 免费档上限 | 免费档 | “订阅已暂停” |

- 降级时：**已有设备和数据保留**；超出上限的设备（按入表时间，保留最早的 N 台）标为“已超额，暂不能被连接”，说明原因和办法；已入表设备在账户里仍能看见和移除。
- 一个账户同时有多个订阅记录时（例如旧的 `canceled` 和新的 `active`），取**权益最高的有效那条**，规则写进单测。
- Stripe 后台设置：Smart Retries 开启（默认）；重试用完后选 `canceled`；开 Customer Portal 的“取消订阅”和“换档位”；税码选 `txcd_10103000`/`txcd_10103001`（SaaS 个人用/商用）或可下载软件的码（Managed Payments 评估时用）；Billing 里不订阅 `invoice.created`。

### 8.4 要产品负责人准备的东西

**账号**
- Supabase 账号和一个**美国东部（`us-east-1`）**的 Pro 项目（先建测试项目和正式项目两个，区域建好不能改）。
- Stripe 账号：现在就能建，先在沙盒里做完；正式模式要先有公司主体（建议美国 LLC 加 EIN 加对公银行账户；个体户加 SSN 也能激活但把个人税号绑进去，【二手】），**注意激活后不能改业务所在国**。
- Google Cloud 项目（OAuth 同意屏幕，可能要应用验证【未核实】）、GitHub OAuth 应用；Apple 开发者账号等以后做 Apple 登录时再办。
- 发邮件的服务（SMTP 或发信 API）：Supabase 内置发信每小时只有 2 封。
- 如果选 Cloudflare 方案：Cloudflare 账号和域名托管。

**密钥和配置（只放进各平台的密钥库，不进仓库）**
- Stripe：受限密钥（只开 Checkout、Portal、订阅和客户读写）、webhook 签名密钥（沙盒和正式各一个）、publishable 密钥（如果以后用到）。
- Supabase：项目地址、publishable 密钥（客户端用）、secret 密钥（只放 Edge Function 或控制面，不放中继节点）。
- 在 Supabase 控制台：迁移 JWT 签名密钥并轮换到 ES256；确认 JWKS 端点有公钥；在 Auth 里配 Redirect URLs、关匿名登录、改邮件模板发 OTP 码。
- OAuth：Google、GitHub 的客户端 ID 和密钥。

**域名和网页**
- 主域名；`app.` 或 `account.` 子域名放登录后的页面（设备审批页 `/activate`、“回 PeerDesk”页、账户设置、删除账户）；`api.` 给 `peerdesk-server` 的 HTTP 接口（如果有）；发信域名的 SPF、DKIM、DMARC 记录。
- 定价页（档位、价格、设备数、中继额度）。
- 状态页或联系邮箱（支持和删除请求用）。

**法律页面和流程**
- 服务条款、隐私政策（列 6.4 的第三方）、退款政策、可接受使用政策（远程桌面容易被拿去干坏事，要写明）；
- 账户删除和数据导出的说明与联系方式；
- Cookie 说明（网页有统计时）；
- 销售税：决定用 Stripe Tax（Basic 或 Complete）还是评估 Managed Payments，向 Stripe 拿 Managed Payments 报价；找税务师确认 SaaS 在各州的应税情况和登记时间点；
- 公司主体、EIN、银行账户；
- 向律师确认删除时限和例外（CCPA、GDPR）。

**要产品负责人拍板的事**
1. 收费胶水代码放 Supabase Edge Functions（TypeScript，约 300 行）还是 Cloudflare Workers（Rust）（第 7 节）；
2. 免费档的设备数和中继额度，付费档的价格和额度；是否做试用（建议第一版不做）；
3. 降档后超额设备的处理（建议保留、标“已超额”、不删）；
4. 无屏幕主机的主流程用审批码还是入表令牌（建议两个都做，审批码为主）；
5. 是否评估 Managed Payments。

## 9. 没查到或没核实的清单

- Supabase 新建项目默认用哪种签名算法；旧 HS256 密钥的确切停用日期（文档没给）；旧 API 密钥的确切弃用日期（“2026 年底”/“Late 2026, TBC”）。
- Supabase 白名单是否接受回环地址任意端口（`http://127.0.0.1:*/…`）：文档没写，要在项目上实测；`/authorize` 的 `code_challenge` 参数写法要抓 supabase-js 请求确认。
- 刷新令牌被超出复用窗口重用时 Supabase 的处理（是否撤销整个会话）。
- Custom Access Token Hook 在哪个套餐可用。
- Supabase Edge Functions 的冷启动、请求体大小、日志保留；Cloudflare Workers 的日志和 Cron；Supabase 备份保留期。
- Google 登录的配置页和应用验证要求；GitHub OAuth 应用的限制。
- Apple 开发者账号价格（页面没写）。
- Stripe Entitlements 的费用；Managed Payments 的费率和是否正式发布；Stripe 官方有没有 Rust SDK；“Fulfill orders”页的正文（只拿到目录）；收款银行账户的具体要求；非美国居民开主体的路径。
- 美国各州对 SaaS 的应税规则；CCPA、GDPR 删除时限和例外；Stripe 隐私页后半部分（只读了前 10 万字符）。
- Supabase OpenAPI 文件只读了前 10 万字符，JWKS 路径在后半部分，没在这份文件里确认（signing-keys 页已确认）。
- 各 crate 只核对了 crates.io 清单里的许可证字段和版本日期，没逐个打开仓库的 LICENSE 文件（GitHub API 在本环境读不到），也没扫间接依赖；`jsonwebtoken` 是否有 `DecodingKey::from_jwk` 要读 docs.rs。
- VS Code tunnel 的无头登录细节；`workers-rs` 是否仍标测试版。
- WASM 里做 HMAC-SHA256 只是按 crate 能编到 WASM 推断，没编译验证。
- Windows 凭据管理器单条凭据大小上限（凭记忆写的约 2560 字节）。

## 10. 来源

- Supabase：[JWT Signing Keys](https://supabase.com/docs/guides/auth/signing-keys)、[JWTs](https://supabase.com/docs/guides/auth/jwts)、[JWT Claims Reference](https://supabase.com/docs/guides/auth/jwt-fields)、[Sessions](https://supabase.com/docs/guides/auth/sessions)、[PKCE flow](https://supabase.com/docs/guides/auth/sessions/pkce-flow)、[Native mobile deep linking](https://supabase.com/docs/guides/auth/native-mobile-deep-linking)、[Redirect URLs](https://supabase.com/docs/guides/auth/redirect-urls)、[Passwordless email](https://supabase.com/docs/guides/auth/auth-email-passwordless)、[Apple](https://supabase.com/docs/guides/auth/social-login/auth-apple)、[GitHub](https://supabase.com/docs/guides/auth/social-login/auth-github)、[Identity linking](https://supabase.com/docs/guides/auth/auth-identity-linking)、[Rate limits](https://supabase.com/docs/guides/auth/rate-limits)、[OAuth 2.1 flows](https://supabase.com/docs/guides/auth/oauth-server/oauth-flows)、[Custom Access Token Hook](https://supabase.com/docs/guides/auth/auth-hooks/custom-access-token-hook)、[Auth OpenAPI](https://raw.githubusercontent.com/supabase/auth/master/openapi.yaml)、[RLS](https://supabase.com/docs/guides/database/postgres/row-level-security)、[Regions](https://supabase.com/docs/guides/platform/regions)、[Stripe webhooks 示例](https://supabase.com/docs/guides/functions/examples/stripe-webhooks)、[Edge Functions limits](https://supabase.com/docs/guides/functions/limits)、[Edge Functions secrets](https://supabase.com/docs/guides/functions/secrets)、[Pricing](https://supabase.com/pricing)、[DPA](https://supabase.com/legal/dpa)、[deleteUser](https://supabase.com/docs/reference/javascript/auth-admin-deleteuser)。
- Stripe：[How subscriptions work](https://docs.stripe.com/billing/subscriptions/overview)、[Subscription webhooks](https://docs.stripe.com/billing/subscriptions/webhooks)、[Webhooks](https://docs.stripe.com/webhooks)、[Smart Retries](https://docs.stripe.com/billing/revenue-recovery/smart-retries)、[Customer Portal](https://docs.stripe.com/customer-management)、[Create Checkout Session](https://docs.stripe.com/api/checkout/sessions/create)、[Create portal session](https://docs.stripe.com/api/customer_portal/sessions/create)、[Entitlements](https://docs.stripe.com/billing/entitlements.md?dashboard-or-api=api)、[API keys](https://docs.stripe.com/keys)、[Set up your account](https://docs.stripe.com/get-started/account/activate)、[Pricing](https://stripe.com/pricing)、[Tax in the US](https://docs.stripe.com/tax/supported-countries/united-states)、[Managed Payments](https://docs.stripe.com/payments/managed-payments)、[Managed Payments eligibility](https://docs.stripe.com/payments/managed-payments/eligibility)、[Legal](https://stripe.com/privacy-center/legal)。
- 标准和业内：[RFC 8252](https://www.rfc-editor.org/rfc/rfc8252.html)、[RFC 8628](https://www.rfc-editor.org/rfc/rfc8628.html)、[gh auth login](https://cli.github.com/manual/gh_auth_login)、[Tailscale CLI](https://tailscale.com/kb/1080/cli)、[VS Code tunnels](https://code.visualstudio.com/docs/remote/tunnels)。
- Cloudflare：[Rust on Workers](https://developers.cloudflare.com/workers/languages/rust/)、[Workers pricing](https://developers.cloudflare.com/workers/platform/pricing/)、[Workers limits](https://developers.cloudflare.com/workers/platform/limits/)、[Secrets](https://developers.cloudflare.com/workers/configuration/secrets/)、[Queues pricing](https://developers.cloudflare.com/queues/platform/pricing/)、[workers-rs README 和 LICENSE](https://github.com/cloudflare/workers-rs)。
- crate：crates.io 的 `jsonwebtoken`、`async-stripe`、`stripe-rust`、`keyring`、`worker`、`jwks`、`axum-jwks`、`hmac`、`sha2`、`oauth2`、`webbrowser` 清单（2026-11-28 读取）；[jsonwebtoken README 和更新日志](https://github.com/Keats/jsonwebtoken)、[async-stripe README](https://github.com/arlyon/async-stripe)、[keyring-rs README](https://github.com/open-source-cooperative/keyring-rs)。
- 二手：[Supabase 迁移到新 API 密钥](https://supabase.com/docs/guides/getting-started/migrating-to-new-api-keys)、[stripe-sync-engine](https://supabase.github.io/stripe-sync-engine/edge-function)、[Stripe 个体户无 EIN 注册](https://support.stripe.com/questions/signing-up-for-stripe-as-a-sole-proprietor-without-employer-id-number)。
- 仓库内：`git show devices:docs/research/N-devices.md`（第 2.1、3.1、3.2 节）、`crates/client/src/keychain.rs`、`crates/client/Cargo.toml`。
