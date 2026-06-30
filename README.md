# ServerStatus-RustL

轻量 VPS 状态面板。当前维护分支在原 ServerStatus-Rust 基础上增加了后台管理、VPS 到期信息、动态 Agent 接入、健康告警和 Telegram/Bark 通知。

本项目只做监控和告警，不包含 SSH、远程 shell、远程命令下发、终端或任务执行能力。

## 主要功能

- VPS 状态主页：在线状态、CPU、内存、硬盘、流量、负载、网络和剩余到期天数。
- 后台管理：服务器、分组、告警规则、通知方式、接入地址、到期提醒和账号密码。
- 动态接入：后台生成一键 Agent 接入命令，自动生成接入密钥，安装 URL 只携带短期接入令牌。
- 到期管理：支持到期日期、永久、免费、自动续期周期和到期提醒。
- 健康告警：支持离线、CPU、内存、硬盘和负载持续超阈值提醒。
- 通知通道：Telegram Bot 与 Bark，可用于到期提醒和健康告警。
- Docker 部署：默认使用 GitHub Actions 构建并发布到 GHCR 的预构建镜像。

## 安全边界

- 后台 API 使用 JWT 保护，未登录或错误 token 应返回 `401`。
- 登录接口会对连续失败做短时间限速，避免后台密码被高频尝试。
- 前端可读取的后台配置会脱敏，不返回 Agent 密码、接入组密码、`admin_pass`、`jwt_secret`、Telegram token 或 Bark device key。
- 后台复制的一键接入 URL 不再包含真实 Agent/接入组密码，只包含默认 24 小时有效的安装令牌。
- 静态页面会返回基础安全响应头，例如 `X-Content-Type-Options`、`Referrer-Policy` 和 `X-Frame-Options`。这些响应头不影响 Nginx Proxy Manager 反向代理。
- 后台修改的运行时配置会写入 `runtime/admin-overrides.json`，不要提交到 Git。
- 不要提交 `runtime/`、`admin-overrides.json`、`stats.json`、真实后台密码、JWT 密钥、通知 token 或接入密钥。

## 快速部署

推荐使用 Docker Compose。默认 `docker-compose.yml` 会直接拉取预构建镜像：

```text
ghcr.io/luke9570/serverstatus-rustl:latest
```

首次部署：

```bash
git clone https://github.com/Luke9570/ServerStatus-RustL.git
cd ServerStatus-RustL

mkdir -p runtime
docker network create proxy 2>/dev/null || true

docker compose pull
docker compose up -d
docker compose logs -f stat_server
```

服务默认监听：

- 面板：`http://服务器IP:8080/`
- 后台：`http://服务器IP:8080/admin`
- gRPC/兼容 Agent 入口：`9394`

`config.toml` 中 `admin_pass` 留空且还没有通过后台保存过密码时，服务启动日志会生成随机后台密码。只要 `config.toml` 已配置 `admin_pass`，或 `runtime/admin-overrides.json` 中已有后台密码哈希，启动日志就不会再打印临时密码。

`jwt_secret` 留空时会生成本次启动可用的随机密钥。正式使用建议改成强随机值：

```bash
openssl rand -base64 32
```

后台登录成功后，账号密码修改会写入 `runtime/admin-overrides.json`。密码只保存 PBKDF2 哈希，不会明文保存。

`docker-compose.yml` 默认仍映射 `9394:9394`，方便需要 gRPC/兼容入口的部署。只使用 Web 面板和 HTTP `/report` 上报时，可以自行移除这条端口映射，反向代理只需要转发 `8080`。

## 日常更新

VPS 上更新代码和镜像：

```bash
cd /home/docker_data/ServerStatus-RustL

cp config.toml config.toml.local.bak
cp Dockerfile Dockerfile.local.bak 2>/dev/null || true

git restore Dockerfile
git fetch origin
git switch -C main origin/main

docker compose pull
docker compose up -d --force-recreate
docker compose logs -f stat_server
```

日常更新不要使用 `docker compose build --no-cache`。该命令会让 Rust 依赖接近从零编译，在小 VPS 上可能非常慢。

如需本地源码构建镜像调试，再使用：

```bash
docker compose -f docker-compose.yml -f docker-compose.build.yml build
docker compose -f docker-compose.yml -f docker-compose.build.yml up -d --force-recreate
```

如果要查看详细构建过程，`--progress` 是 compose 全局参数，写法是：

```bash
docker compose --progress plain -f docker-compose.yml -f docker-compose.build.yml build
```

## Nginx Proxy Manager 反向代理

如果 NPM 也是 Docker 容器，建议让 NPM 和本项目容器加入同一个 Docker network。当前 compose 已配置外部网络 `proxy`：

```bash
docker network create proxy 2>/dev/null || true
docker compose up -d
```

NPM Proxy Host 建议填写：

```text
Forward Hostname / IP: stat_server
Forward Port: 8080
Scheme: http
```

如果 NPM 容器无法解析 `stat_server`，先确认两个容器都在 `proxy` 网络：

```bash
docker inspect -f '{{range $name, $_ := .NetworkSettings.Networks}}{{println $name}}{{end}}' npm-app-1
docker inspect -f '{{range $name, $_ := .NetworkSettings.Networks}}{{println $name}}{{end}}' stat_server
```

## 面板地址与 Agent 上报地址

后台“设置”里有两个地址：

- 面板访问地址：用户打开主页、后台和拉取 `/i` 安装脚本的地址，可以走 CDN。
- Agent 上报地址：Agent 提交 `/report` 的地址，建议使用不经过 CDN 的源站地址。

后台复制的一键接入命令会从面板访问地址拉取安装脚本；安装脚本内部启动 `stat_client` 时才会使用 Agent 上报地址。

如果 Agent 上报域名在 Nginx Proxy Manager 中开启了 Force SSL，后台“Agent 上报地址”也应填写 `https://...`，并重新复制接入命令安装 Agent，不要依赖旧 HTTP 命令被 301 跳转。

这适合以下部署方式：

```text
用户浏览器/安装脚本 -> CDN 域名 -> NPM -> stat_server
Agent 上报          -> 源站域名/IP -> stat_server
```

## Agent 接入

推荐在后台“接入服务器”中复制一键接入命令。后台会自动生成接入密钥和参数。复制出来的安装地址使用短期接入令牌，真实接入密钥不会出现在浏览器地址栏、CDN 日志或 NPM 访问日志中。

安装令牌默认 24 小时有效。过期后重新在后台复制接入命令即可。旧版 `pass=` 接入 URL 仍保留兼容，但不建议继续手动传播。

手动 HTTP 上报示例：

```bash
./stat_client -a "http://127.0.0.1:8080/report" -u h1 -p p1
```

动态接入组示例：

```bash
./stat_client -a "http://127.0.0.1:8080/report" -g default -p "<接入密钥>" --alias "$(hostname)"
```

常用参数：

```text
--disable-ping      停用三网延迟和丢包率探测
--disable-tupd      不上报 TCP/UDP/进程数/线程数
--disable-extra     不上报系统信息和 IP 信息
--vnstat            使用 vnstat 统计流量
--location          手动指定位置
--type              手动指定主机类型，例如 kvm、openvz、lxc
-w, --weight        排序权重
```

位置、显示名和类型可以留空自动识别；类型会优先使用 agent 探测到的虚拟化/容器环境，例如 kvm、openvz、lxc、docker，不会用 CPU 架构 x86_64/aarch64 代替。

## 到期与告警

到期信息可以在后台维护，也可以在 `config.toml` 中配置：

```toml
{ name = "vps-1", password = "p1", billing = { end_date = "2026-12-31", auto_renewal = "1", cycle = "Year", amount = "200EUR" } }
{ name = "lifetime", password = "p1", expire = "permanent", billing = { amount = "free" } }
```

告警规则建议在后台配置。支持：

- 离线超过指定时间后提醒，避免短暂丢包误报。
- CPU、内存、硬盘使用率持续超阈值提醒。
- 1/5/15 分钟负载持续超阈值提醒。
- 按服务器或分组应用规则。
- 选择已启用的 Telegram 或 Bark 通知方式。

## Release 与安装脚本

后台生成的一键安装脚本默认从当前仓库 Release 下载 `stat_client`，不会回退到上游仓库。

Release 中需要包含对应架构的 Agent 压缩包，例如：

```text
client-x86_64-unknown-linux-musl.zip
client-aarch64-unknown-linux-musl.zip
```

如果要临时指定 Release 源或 tag：

```bash
SSR_RELEASE_REPO=Luke9570/ServerStatus-RustL \
SSR_RELEASE_TAG=v1.8.2 \
curl -fsSL "https://example.com/i?..." | bash
```

## 本地开发检查

```bash
cargo check -p stat_server --locked
cargo test -p stat_server --locked
cargo build -p stat_server -p stat_client --locked
./target/debug/stat_server -c config.toml -t
git diff --check
```

修改 `web/` 下的主页或后台静态资源后，需要重新构建服务端，因为静态资源会嵌入到 `stat_server` 二进制中。

## 运行时文件

以下文件只属于本机运行环境，不要提交：

```text
runtime/
admin-overrides.json
stats.json
tls/
target/
```
