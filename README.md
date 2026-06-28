# ServerStatus-RustL

轻量 VPS 状态面板，当前主线重点增强到期管理、后台配置、节点健康告警和 Telegram/Bark 通知，保持纯监控用途。

## 功能概览

- Rust 服务端与客户端，支持 HTTP/gRPC 上报。
- 主页展示在线状态、流量、负载、CPU、内存、硬盘和 VPS 剩余天数。
- Nezha 风格到期管理：到期日期、永久/免费、自动续期周期和剩余天数展示。
- 后台管理：服务器、服务器分组、告警规则、通知方式、接入地址、到期提醒和账号密码修改。
- 动态接入命令：后台登录后生成 Agent 一键接入脚本，支持单独配置面板访问地址和 Agent 上报地址。
- 告警规则：离线、CPU、内存、硬盘、负载持续超阈值提醒，可限定服务器或服务器分组。
- 通知方式：Telegram 与 Bark，支持到期提醒模板和健康告警模板。
- 运行时覆盖配置写入本地 `admin-overrides.json`，不需要修改 `config.toml` 才能调整大部分后台配置。

安全边界：

- 不包含 SSH、远程 shell、远程任务执行、终端或命令下发能力。
- 后台 API 使用 JWT 保护，未登录访问应返回 `401`。
- `/api/admin/config.json` 和 `/api/admin/settings` 给前端的数据会脱敏，不返回 agent/group password、`admin_pass`、`jwt_secret`、Telegram token、Bark device key。
- 不要提交 `admin-overrides.json`、`runtime/`、真实 `admin_pass`、`jwt_secret`、Telegram token、Bark device key。

## 项目结构

```text
server/                 Rust 服务端
client/                 Rust Agent
common/                 gRPC/protobuf 公共定义
web/                    已构建的主页与后台静态资源
web/jinja/              Agent 一键接入脚本模板
scripts/                systemd 安装/管理辅助脚本
systemd/                stat_server/stat_client systemd 示例
config.toml             示例配置
docker-compose.yml      GHCR 预构建镜像 Docker Compose 示例
docker-compose.build.yml 本地源码构建 Docker Compose override
```

关键文件：

- `server/src/admin.rs`：后台覆盖配置、密码哈希、运行时设置持久化。
- `server/src/http.rs`：HTTP 页面与后台受保护 API。
- `server/src/jwt.rs`：后台登录 JWT。
- `server/src/stats.rs`：节点状态、覆盖配置、排序、到期提醒和健康告警。
- `server/src/expiry.rs`：到期日期解析、自动续期推算、状态文案。
- `web/admin.html`、`web/static/js/admin.js`、`web/static/css/admin.css`：后台 UI。
- `web/static/js/expiry.js`、`web/static/css/expiry.css`：主页到期信息展示。

## 本地验证

```bash
cargo check -p stat_server --locked
cargo test -p stat_server --locked
cargo build -p stat_server -p stat_client --locked
./target/debug/stat_server -c config.toml -t
```

启动服务端：

```bash
./target/debug/stat_server -c config.toml
```

示例 Agent：

```bash
./target/debug/stat_client \
  -a http://127.0.0.1:8080/report \
  -g renew \
  -p pp \
  --alias demo-agent-1 \
  --disable-ping \
  --disable-extra \
  --disable-tupd \
  --interval 1
```

访问：

- 主页：http://127.0.0.1:8080/
- 后台：http://127.0.0.1:8080/admin

`config.toml` 中 `admin_pass` 留空时，服务启动会在日志中生成随机后台密码；`jwt_secret` 留空时会为本次启动生成随机密钥。正式部署请设置强随机值。已配置的 `admin_pass` 和 `jwt_secret` 不会明文输出到日志。

```bash
openssl rand -base64 32
```

## 配置要点

### 后台登录

```toml
jwt_secret = ""
admin_user = "admin"
admin_pass = ""
```

后台修改用户名或密码后，会写入 `admin-overrides.json`；新密码只保存 PBKDF2 哈希，不会明文写入 `config.toml`。账号变更会提升会话版本，使旧 JWT 失效。后台登录 JWT 默认 3 天过期。

### 静态服务器配置

```toml
hosts = [
  { name = "h1", password = "p1", alias = "n1", location = "us", type = "kvm", labels = "spec=2C/4G/60G;" },
]
```

### 动态接入组

```toml
hosts_group = [
  { gid = "renew", password = "pp", location = "us", type = "kvm", labels = "spec=2C/4G/60G;" },
]
```

后台“接入服务器”会自动生成接入密钥和一键脚本。若面板经过 CDN，而 Agent 不能通过 CDN 上报，请在后台“设置”里分别填写：

- 面板访问地址：给用户浏览器访问后台/主页。
- Agent 上报地址：给 Agent 上报 `/report`。

后台复制的一键接入命令会从“面板访问地址”拉取 `/i` 安装脚本；生成的脚本内部 `stat_client -a` 才会使用“Agent 上报地址”。因此面板地址可以经过 CDN，Agent 上报地址应填写不经过 CDN 的源站地址。

接入命令中的“位置”和“类型”可留空，留空时优先使用 Agent 上报的系统/IP 信息自动识别；只有需要手动覆盖时才填写。

### VPS 到期信息

可使用以下来源之一：

- `host.expire`
- `host.billing.end_date`
- `labels` 中的 `ndd=2026-12-31`

自动续期示例：

```toml
{ name = "vps-1", password = "p1", billing = { end_date = "2026-12-31", auto_renewal = "1", cycle = "Year", amount = "200EUR" } }
```

永久或免费：

```toml
{ name = "lifetime", password = "p1", expire = "permanent", billing = { amount = "free" } }
```

后台也可以直接覆盖服务器的到期类型、周期、金额和提醒开关。

### 到期提醒

```toml
[expire_notify]
enabled = false
days = [30, 14, 7, 3, 1, 0]
interval = 86400
```

提醒会复用已启用的 Telegram/Bark 通道。

### 健康告警

告警规则在后台配置，不需要写入 `config.toml`。支持：

- 离线超过指定秒数。
- CPU/内存/硬盘使用率持续超过阈值。
- 1/5/15 分钟负载持续超过阈值。
- 限定到单台服务器或服务器分组。
- 选择已启用的 Telegram/Bark 通知方式。

离线告警会使用持续时间，避免短时丢包或抖动导致频繁误报。

### Telegram

```toml
[tgbot]
enabled = false
bot_token = "<tg bot token>"
chat_id = "<chat id>"
title = "ServerStatus"
expire_tpl = """
{{config.title}}
<pre>VPS 到期提醒: {{host.alias}}</pre>
<pre>到期: {{host.expire.date}} / {{host.expire.label}}</pre>
"""
health_tpl = """
{{config.title}}
<pre>{{host.custom}}</pre>
"""
```

### Bark

```toml
[bark]
enabled = false
server = "https://api.day.app"
device_key = "<bark device key>"
title = "ServerStatus"
group = "ServerStatus"
expire_tpl = """
VPS 到期提醒: {{host.alias}}
到期: {{host.expire.date}} / {{host.expire.label}}
"""
health_tpl = """
{{host.custom}}
"""
```

后台保存 Telegram/Bark 时，token/device key 留空表示保持原值。

## Agent

HTTP 上报：

```bash
./stat_client -a "http://127.0.0.1:8080/report" -u h1 -p p1
```

动态接入组：

```bash
./stat_client -a "http://127.0.0.1:8080/report" -g renew -p pp --alias "$(hostname)"
```

常用参数：

```text
--disable-ping      停用三网延时和丢包率探测
--disable-tupd      不上报 TCP/UDP/进程数/线程数
--disable-extra     不上报系统信息和 IP 信息
--vnstat            使用 vnstat 统计流量
--location          手动指定位置
--type              手动指定架构/类型
-w, --weight        排序权重
```

后台生成的一键脚本来自 `web/jinja/client-init.jinja.sh`。默认从 GitHub Releases 下载 `stat_client`，可通过环境变量覆盖下载仓库：

```bash
SSR_RELEASE_REPO=Luke9570/ServerStatus-RustL curl -fsSL "https://example.com/i?..." | bash
```

安装脚本只会从当前维护仓库的 Release 下载，不会回退到上游仓库。Release 中必须存在与当前版本匹配的 Agent 产物，例如 `client-x86_64-unknown-linux-musl.zip` 或 `client-aarch64-unknown-linux-musl.zip`；仓库内的 GitHub Actions 会在推送 `v*` tag 时自动创建这些产物。

如需临时使用其它 tag，可额外设置 `SSR_RELEASE_TAG`：

```bash
SSR_RELEASE_TAG=v1.8.2 SSR_RELEASE_REPO=Luke9570/ServerStatus-RustL curl -fsSL "https://example.com/i?..." | bash
```

`scripts/one-touch.sh` 与 `scripts/status.sh` 也默认使用 `Luke9570/ServerStatus-RustL` 的 Release；如需使用其它 Release 源，同样可设置 `SSR_RELEASE_REPO`。

## 自托管部署

### 二进制 + systemd

```bash
cargo build -p stat_server -p stat_client --release --locked
install -Dm755 target/release/stat_server /opt/ServerStatus/stat_server
install -Dm755 target/release/stat_client /opt/ServerStatus/stat_client
install -Dm644 config.toml /opt/ServerStatus/config.toml
install -Dm644 systemd/stat_server.service /etc/systemd/system/stat_server.service
systemctl daemon-reload
systemctl enable --now stat_server
```

按需修改 `systemd/stat_client.service` 后安装到 Agent 机器。

### Docker Compose

当前 Compose 默认使用 GitHub Actions 发布到 GHCR 的预构建镜像，并把运行时文件放在 `runtime/`：

```bash
mkdir -p runtime
docker network create proxy 2>/dev/null || true
docker compose pull
docker compose up -d
```

VPS 上从 GitHub 更新当前主线代码并拉取新镜像：

```bash
cd /path/to/ServerStatus-RustL
git fetch origin
git checkout main
git pull --ff-only origin main
docker compose pull
docker compose up -d --force-recreate
docker compose logs -f
```

如果 `docker compose pull` 提示没有权限，请到 GitHub Packages 将 `serverstatus-rustl` 容器包设为 Public；或在 VPS 使用有 `read:packages` 权限的 token 执行 `docker login ghcr.io`。

日常更新不需要在 VPS 上 `docker compose build`。如需在本机或 VPS 从源码构建调试镜像，再使用：

```bash
docker compose -f docker-compose.yml -f docker-compose.build.yml build
docker compose -f docker-compose.yml -f docker-compose.build.yml up -d --force-recreate
```

不要日常使用 `docker compose build --no-cache`；该参数会强制 Docker 不复用构建层，Rust 依赖会接近从零编译。项目 Dockerfile 已启用 BuildKit cache mount 缓存 Cargo registry/git/target，普通本地构建会明显快得多。只有怀疑基础镜像或依赖缓存损坏时才使用 `--no-cache`。

如果你的 Compose 服务名不是默认值，请把最后几条命令中的服务名按实际 `docker compose ps` 输出调整。更新后建议重新进入后台复制 Agent 接入命令；旧机器上如果已经装过失败的 Agent 服务，可先在 Agent 机器执行：

```bash
systemctl disable --now stat_client.service
```

然后重新运行后台复制的新接入命令。

`runtime/` 中可能出现：

- `admin-overrides.json`
- `stats.json`
- 其它运行时状态

这些文件包含本地配置或状态，不应提交到 Git。
Linux/macOS 下后台覆盖配置会以 `0600` 权限写入，避免通知 token、接入密钥和密码哈希被其他本机用户读取。

## 前端与静态资源

主页和后台静态资源位于 `web/`，由 `server/src/assets.rs` 嵌入到 `stat_server` 二进制中。修改 `web/admin.html`、`web/static/js/*.js`、`web/static/css/*.css` 后，需要重新构建服务端：

```bash
cargo build -p stat_server --locked
```

## 运行时文件

不要提交以下文件或目录：

```text
admin-overrides.json
runtime/
stats.json
tls/
target/
```

## 维护检查

提交前建议运行：

```bash
cargo check -p stat_server --locked
cargo test -p stat_server --locked
cargo build -p stat_server -p stat_client --locked
git diff --check
```

后台接口安全检查：

```bash
curl -i http://127.0.0.1:8080/api/admin/settings
```

未登录应返回 `401`。

## 致谢

- https://github.com/zdz/ServerStatus-Rust
- https://github.com/BotoX/ServerStatus
- https://github.com/cppla/ServerStatus
- https://github.com/cokemine/ServerStatus-Hotaru
