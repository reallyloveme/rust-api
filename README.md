# 本地网络转发平台

把本地前端的 API 请求反向代理到生产/测试后端，按浏览器 `Origin` 区分多前端，并提供 Web 管理台做配置与联调。

---

## 一、系统文档

### 1.1 定位与目标

| 场景 | 说明 |
|------|------|
| 本地联调 | 前端跑在本机，后端用测试/生产环境，无需改后端 CORS 或部署本地后端 |
| 多前端并行 | 不同端口的前端（如 8080 / 5173）各自一套环境与规则 |
| 可视化运维 | 管理台切换环境、改规则、看请求日志与入参/响应 |

### 1.2 技术栈

- **语言 / 运行时**：Rust、Tokio
- **HTTP**：Axum（入口）、reqwest + rustls（上游）
- **配置**：`config.yaml`（serde_yaml），管理台保存后写回文件
- **管理台**：`static/admin.html` 编译期 `include_str!` 嵌入二进制

### 1.3 架构概览

```
浏览器前端 (Origin: http://localhost:8080)
        │
        │  API 直连 http://127.0.0.1:8787/...
        ▼
┌───────────────────────────────────────┐
│  rust-api                             │
│  ├─ CORS（按前端 Origins 热更新）      │
│  ├─ /health                           │
│  ├─ /__admin  管理台 + 管理 API         │
│  └─ fallback  反向代理                 │
│       ├─ Origin → 前端配置             │
│       ├─ 路径最长前缀 → 规则           │
│       └─ 转发到 target + 可选去前缀    │
└───────────────────────────────────────┘
        │
        ▼
   上游后端 (dev / test / prod)
```

### 1.4 核心行为

1. **前端识别**：优先读请求头 `Origin`，否则从 `Referer` 解析；匹配某前端的 `origins` 列表。
2. **无 Origin**：仅配置了 **一个** 前端时可用（如 curl）；多个前端时返回 `missing origin`。
3. **规则匹配**：在该前端的 `active_env` 下，按 `match_prefix` **最长前缀优先**；未命中 → `404`。
4. **路径改写**：`strip_prefix: true` 时去掉匹配前缀再拼到 `target`。
5. **请求头**：过滤 hop-by-hop；可按规则 `headers` 注入。
6. **Cookie 改写**（前端开关 `rewrite_cookies`）：去掉 `Set-Cookie` 的 `Domain`；若 Origin 为 `http://` 再去掉 `Secure`。
7. **超时**：上游连接 10s / 整体 30s。
8. **日志**：内存环形缓冲（约 500 条），含入参摘要、响应摘要；管理台按前端筛选。

### 1.5 目录结构

```
rust-api/
├── src/
│   ├── main.rs      # 路由装配、健康检查
│   ├── config.rs    # 配置模型读写与校验
│   ├── state.rs     # 运行时状态、Origin 解析、规则匹配
│   ├── proxy.rs     # 反向代理、摘要、Cookie 改写
│   ├── cors.rs      # 动态 CORS
│   └── admin.rs     # 管理台页面与 API
├── static/admin.html
├── config.yaml
├── run.ps1          # Windows 启停端口后 cargo run
└── .cargo/config.toml
```

### 1.6 HTTP 入口

| 路径 | 说明 |
|------|------|
| `GET /health` | 健康检查，返回 listen、前端数量与 id |
| `GET /__admin` | 管理台页面 |
| `/__admin/api/*` | 管理 API（前端/环境/规则/日志/试连/重载） |
| 其余任意方法 | 反向代理（fallback） |

### 1.7 明确不做（当前）

WebSocket、`Location` 改写、响应流式透传、管理台鉴权、流量重放。

---

## 二、使用文档

### 2.1 环境要求

- Rust（推荐 stable）
- Windows 若用 **GNU** 工具链：需 MinGW（如 WinLibs）的 `gcc` 在 PATH；本仓库 `.cargo/config.toml` 可指定 linker

### 2.2 启动

```bash
cargo run
```

Windows 推荐（先结束占用端口的进程再启动）：

```powershell
.\run.ps1
.\run.ps1 -Port 8787
```

若无法执行脚本：

```powershell
Set-ExecutionPolicy -Scope CurrentUser RemoteSigned
```

指定配置文件：

```powershell
$env:CONFIG_PATH="config.yaml"
cargo run
```

| 地址 | 用途 |
|------|------|
| http://127.0.0.1:8787 | 代理入口 |
| http://127.0.0.1:8787/health | 健康检查 |
| http://127.0.0.1:8787/__admin | 管理台 |

> 修改 `static/admin.html` 后必须重新编译（`include_str!` 嵌入），再用 `.\run.ps1` 启动并硬刷新浏览器。

### 2.3 配置 `config.yaml`

按 **前端 Origin** 分流；每个前端独立 `active_env`、`envs`、`rules`。

```yaml
listen: "127.0.0.1:8787"

frontends:
  web8080:
    label: 本地8080
    origins:
      - "http://localhost:8080"
      - "http://127.0.0.1:8080"
    active_env: dev
    rewrite_cookies: false
    envs:
      dev:
        label: 开发
        rules:
          - name: api
            match_prefix: /api
            target: http://172.20.127.168:8885
            strip_prefix: true
            headers: {}
```

#### 字段说明

| 字段 | 说明 |
|------|------|
| `listen` | 监听地址 |
| `frontends.<id>` | 前端配置，id 自定义（如 `web8080`） |
| `label` | 管理台显示名 |
| `origins` | 匹配的 Origin 列表（`localhost` 与 `127.0.0.1` 不同，需都写） |
| `active_env` | 当前生效的环境 id |
| `rewrite_cookies` | 是否改写上游 `Set-Cookie` |
| `envs.<id>.rules` | 该环境下的转发规则 |
| `rules[].match_prefix` | 路径前缀 |
| `rules[].target` | 上游基址（不含被 strip 的前缀路径时按规则拼接） |
| `rules[].strip_prefix` | 是否去掉 `match_prefix` 再转发 |
| `rules[].headers` | 注入到上游的额外请求头 |

管理台保存或「从文件重载」会读写该文件；CORS 允许源会与各前端 `origins` 合并，改 Origins 后 **无需重启** 即可热更新。

### 2.4 前端联调（重要）

**推荐：前端 API 直连本代理**，浏览器会带正确 `Origin`，多前端分流才可靠：

```ts
// .env.development
VITE_API_BASE=http://127.0.0.1:8787
```

页面在 `http://localhost:8080` 时，Origin 为该地址，须写在对应前端的 `origins` 中。

**不推荐**只靠 Vite `server.proxy` 转到本服务：Node 转发常丢失浏览器 `Origin`，易出现 `missing origin`。若必须代理，请保留 `Origin`，或只配置一个前端。

### 2.5 管理台操作

打开 http://127.0.0.1:8787/__admin

1. **前端 Tab**：切换 / 新建 / 复制 / 删除前端；编辑 Origins、Cookie 改写并保存。
2. **后端环境**：切换 `active_env`；新建 / 复制 / 删除环境。
3. **转发规则**：编辑名称、前缀、目标、是否去前缀、注入头；保存规则；可对规则「试连」（上游 GET 探测）。
4. **最近请求**：
   - 按状态码筛选、清空、约每 5 秒自动刷新
   - 左侧 ▶ 展开查看 Origin / 环境 / 规则 / 上游 / **入参** / **响应**，可复制
   - 时间显示为北京时间
5. **从文件重载**：丢弃未保存的界面状态，重新读 `config.yaml`。

### 2.6 命令行验证

```bash
# 健康检查
curl http://127.0.0.1:8787/health

# 多前端时必须带 Origin
curl -H "Origin: http://localhost:8080" http://127.0.0.1:8787/api/your-path
```

### 2.7 常见问题

| 现象 | 处理 |
|------|------|
| `missing origin` | 前端改为直连代理；或只保留一个前端；curl 加 `-H "Origin: ..."` |
| `unknown frontend origin` | 把页面 Origin 写入该前端的 `origins`（注意 localhost ≠ 127.0.0.1） |
| `no matching proxy rule` | 检查当前环境规则前缀是否覆盖请求路径 |
| 端口占用 (10048) | 用 `.\run.ps1`，或手动结束占用 8787 的进程 |
| 管理台改了没变化 | 需 `cargo run` / `.\run.ps1` 重新编译，并硬刷新浏览器 |
| 本地登不上（Cookie） | 对应前端开启 `rewrite_cookies` |
| 展开详情被冲掉 | 已尽量在刷新时保留展开态；仍异常时可先收起再展开，或点「刷新日志」 |

### 2.8 推荐联调流程

1. `.\run.ps1` 启动服务，打开管理台。
2. 为本地前端建/选好前端配置，填好 Origins，选好环境与规则。
3. 前端 `VITE_API_BASE`（或等价配置）指向 `http://127.0.0.1:8787`。
4. 浏览器访问本地页面，在管理台日志里确认命中规则与上下游摘要。
5. 需要登录态时打开 `rewrite_cookies`。
