#!/bin/bash
# ServerStatus-RustL telemetry agent init script

export SSR_PASS={{pass}}
export SSR_UID={{uid}}
export SSR_GID={{gid}}
export SSR_ALIAS={{alias}}
export SSR_SCHEME={{scheme}}
export SSR_DOMAIN={{domain}}
export SSR_SERVER_URL={{server_url}}
export SSR_VNSTAT={{vnstat}}
export SSR_WEIGHT={{weight}}
export SSR_PKG_VERSION={{pkg_version}}
export SSR_CLIENT_OPTS={{client_opts_export}}
export SSR_WORKSPACE={{workspace}}
export SSR_CN={{cn}}
export SSR_RELEASE_REPO=${SSR_RELEASE_REPO:-Luke9570/ServerStatus-RustL}
export SSR_RELEASE_TAG=${SSR_RELEASE_TAG:-v{{pkg_version}}}

Info="\033[32m[info]\033[0m"
Error="\033[31m[err]\033[0m"

mkdir -p "${SSR_WORKSPACE}"
cd "${SSR_WORKSPACE}"

if [ "${DBG}" = "1" ]; then
    set -x
fi

function say() {
    printf "${Info} ssr-client-init: %s\n" "$1"
}

function err() {
    printf "${Error} ssr-client-init: %s\n" "$1" >&2
    exit 1
}

function check_cmd() {
    command -v "$1" > /dev/null 2>&1
}

function need_cmd() {
    if ! check_cmd "$1"; then
        err "need '$1' (command not found)"
    fi
}

# check arch
function check_arch() {
    need_cmd uname

    case $(uname -m) in
        x86_64)
            arch=x86_64
            target=x86_64-unknown-linux-musl
        ;;
        aarch64 | aarch64_be | arm64 | armv8b | armv8l)
            arch=aarch64
            target=aarch64-unknown-linux-musl
        ;;
        armv7l | armv7)
            arch=armv7
            target=armv7-unknown-linux-musleabihf
        ;;
        *)
            err "暂不支持该系统架构"
            exit 1
        ;;
    esac

    say "os arch: ${arch}"
}

function install_deps() {
    say "checking dependencies"

    if [ "${SSR_VNSTAT}" == "true" ]; then
        cmd_deps=("unzip" "wget" "chmod" "vnstat")
    else
        cmd_deps=("unzip" "wget" "chmod")
    fi
    need_deps=""
    for i in "${!cmd_deps[@]}"; do
        cur_dep="${cmd_deps[$i]}"
        if [ ! -x "$(command -v $cur_dep 2>/dev/null)" ]; then
            say "$cur_dep 未安装"
            need_deps="$cur_dep ${need_deps}"
        fi
    done
    if [ "${need_deps}" ]; then
        say "start installing dependencies: ${need_deps}"
        need_pkgs="${need_deps} ca-certificates"

        if [ -x "$(command -v apk 2>/dev/null)" ]; then
            apk update > /dev/null 2>&1
            apk --no-cache add procps iproute2 coreutils ${need_pkgs} > /dev/null 2>&1
        elif [ -x "$(command -v apt-get 2>/dev/null)" ]; then
            apt-get update -y > /dev/null 2>&1
            apt-get install -y ${need_pkgs} > /dev/null 2>&1
        elif [ -x "$(command -v yum 2>/dev/null)" ]; then
            yum install -y ${need_pkgs} > /dev/null 2>&1
        else
            err "未找到合适的包管理工具,请手动安装: ${need_deps}"
            exit 1
        fi
        for i in "${!cmd_deps[@]}"; do
            cur_dep="${cmd_deps[$i]}"
            if [ ! -x "$(command -v $cur_dep)" ]; then
                err "$cur_dep 未成功安装,请尝试手动安装!"
                exit 1
            fi
        done
    fi
}

function download_client_from_repo() {
    repo="$1"
    url="https://github.com/${repo}/releases/download/${SSR_RELEASE_TAG}/client-${target}.zip"
    say "download from ${repo} ${SSR_RELEASE_TAG}"
    if ! wget -qO "client-${target}.zip" "${url}"; then
        say "download failed from ${repo}"
        return 1
    fi
    if ! unzip -tq "client-${target}.zip" > /dev/null 2>&1; then
        say "invalid zip from ${repo}"
        return 1
    fi
    return 0
}

function download_client() {

    cd "${SSR_WORKSPACE}"
    rm -f "client-${target}.zip" "stat_client" "stat_client.service"

    say "start download the stat_client"

    download_client_from_repo "${SSR_RELEASE_REPO}" || err "failed to download stat_client from ${SSR_RELEASE_REPO} ${SSR_RELEASE_TAG}; please publish client-${target}.zip in this GitHub release"

    say "download stat_client succ"

    say "try stop stat_client.service"
    systemctl stop stat_client > /dev/null 2>&1 || true

    say "unzip client-${target}.zip"
    unzip -o client-${target}.zip || err "failed to unzip stat_client package"
    rm -f "stat_client.service"

    [ -f "${SSR_WORKSPACE}/stat_client" ] || err "stat_client not found after unzip"
    chmod +x "${SSR_WORKSPACE}/stat_client" || err "failed to chmod stat_client"
}

function install_client_service() {
    need_cmd cat
    need_cmd systemctl
    need_cmd sleep

    say "start install stat_client.service"

    cat > /etc/systemd/system/stat_client.service <<-'EOF'
[Unit]
Description=ServerStatus-RustL Telemetry Agent
Wants=network-online.target
After=network-online.target

[Service]
User=root
Group=root
Environment="RUST_BACKTRACE=1"
WorkingDirectory={{workspace_exec}}
ExecStart={{stat_client_exec}} {{client_opts}}
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure

[Install]
WantedBy=multi-user.target

EOF

    say "systemctl daemon-reload"
    systemctl daemon-reload
    say "start stat_client.service"
    systemctl start stat_client
    say "enable stat_client.service"
    systemctl enable stat_client

    sleep 2
    say "status stat_client.service"
    systemctl --no-pager --full status stat_client || true

}

check_arch
install_deps
download_client
install_client_service
