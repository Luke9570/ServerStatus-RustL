# Mac 端继续开发提示词

我在 Windows 上 fork 并修改了 `Luke9570/ServerStatus-RustL`，当前分支实现了 Nezha 风格的 VPS 到期时间、自动续期推算、Telegram/Bark 到期通知、后台配置入口和节点权重排序，但明确不实现 SSH/远程命令/任务执行功能。

请你先完整阅读项目结构和最近一次提交，不要重写架构。重点文件：

- `server/src/expiry.rs`：到期日期解析、自动续期推算、状态文案。
- `server/src/admin.rs`：后台覆盖配置，运行时写入本地 `admin-overrides.json`。
- `server/src/http.rs`：后台受保护 API。
- `server/src/jwt.rs`：后台登录 JWT，要求 `scope=admin` 且用户名匹配配置。
- `server/src/config.rs`：配置结构、后台脱敏配置输出、节点权重。
- `server/src/stats.rs`：应用后台覆盖、到期提醒、离线置底和权重排序。
- `server/src/notifier/bark.rs` 与 `server/src/notifier/tgbot.rs`：Bark/Telegram 通知。
- `web/admin.html`、`web/static/js/admin.js`、`web/static/css/admin.css`：后台 UI。
- `web/static/js/expiry.js`、`web/static/css/expiry.css`：主页到期信息展示。

安全边界：

- 不要添加 SSH、远程 shell、远程任务执行、终端、命令下发能力。
- 不要把 `admin-overrides.json`、`runtime/`、真实 `admin_pass`、`jwt_secret`、Telegram token、Bark device key 提交到 GitHub。
- 后台配置接口必须保持登录后才可读取和保存，未登录或错误 token 应返回 401。
- `/api/admin/config.json` 给前端的数据必须保持脱敏，不能包含 agent/group password、admin_pass、jwt_secret。

Mac 上建议先跑：

```bash
cargo check -p stat_server --locked
cargo test -p stat_server --locked
cargo build -p stat_server -p stat_client --locked
./target/debug/stat_server -c config.toml -t
```

如果要本地启动示例：

```bash
./target/debug/stat_server -c config.toml
```

另开两个终端启动 agent：

```bash
./target/debug/stat_client -a http://127.0.0.1:8080/report -u demo-agent-1 -g renew -p pp --alias demo-agent-1 -o 1 --disable-ping --disable-extra --disable-tupd --interval 1
./target/debug/stat_client -a http://127.0.0.1:8080/report -u demo-agent-2 -g g2 -p pp --alias demo-agent-2 -o 1 --disable-ping --disable-extra --disable-tupd --interval 1
```

打开：

- 主页：`http://127.0.0.1:8080/`
- 后台：`http://127.0.0.1:8080/admin`

注意 `config.toml` 里 `admin_pass` 和 `jwt_secret` 留空时，服务每次启动会在日志里生成随机后台密码；正式使用请改成强随机值。
