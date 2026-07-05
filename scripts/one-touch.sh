#!/bin/bash
set -ex

WORKSPACE=/opt/ServerStatus
SSR_RELEASE_REPO=${SSR_RELEASE_REPO:-Luke9570/ServerStatus-RustL}
mkdir -p ${WORKSPACE}
cd ${WORKSPACE}

# 下载
case "$(uname -m)" in
    x86_64)
        OS_TARGET="x86_64-unknown-linux-musl"
    ;;
    aarch64 | aarch64_be | arm64 | armv8b | armv8l)
        OS_TARGET="aarch64-unknown-linux-musl"
    ;;
    armv7l | armv7)
        OS_TARGET="armv7-unknown-linux-musleabihf"
    ;;
    *)
        echo "unsupported arch: $(uname -m)" >&2
        exit 1
    ;;
esac
latest_version=$(curl -m 10 -sL "https://api.github.com/repos/${SSR_RELEASE_REPO}/releases/latest" | grep "tag_name" | head -n 1 | awk -F ":" '{print $2}' | sed 's/\"//g;s/,//g;s/ //g')

wget -qO "server-${OS_TARGET}.zip"  "https://github.com/${SSR_RELEASE_REPO}/releases/download/${latest_version}/server-${OS_TARGET}.zip"
wget -qO "client-${OS_TARGET}.zip"  "https://github.com/${SSR_RELEASE_REPO}/releases/download/${latest_version}/client-${OS_TARGET}.zip"

unzip -o "server-${OS_TARGET}.zip"
unzip -o "client-${OS_TARGET}.zip"

# systemd service
mv -v stat_server.service /etc/systemd/system/stat_server.service
mv -v stat_client.service /etc/systemd/system/stat_client.service

systemctl daemon-reload

# 启动
systemctl start stat_server
systemctl start stat_client

# 状态查看
systemctl status stat_server
systemctl status stat_client

# 使用以下命令开机自启
# systemctl enable stat_server
# systemctl enable stat_client

# 停止
# systemctl stop stat_server
# systemctl stop stat_client

# https://fedoraproject.org/wiki/Systemd/zh-cn
# https://docs.fedoraproject.org/en-US/quick-docs/understanding-and-administering-systemd/index.html

# 修改 /etc/systemd/system/stat_client.service 文件，将IP改为你服务器的IP或你的域名
