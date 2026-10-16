#!/usr/bin/env bash
# 全项目唯一的版本号：根 Cargo.toml 的 [workspace.package] version。
#   scripts/version.sh get            打印版本号
#   scripts/version.sh check v0.2.9   标签与各清单一致才返回 0（release.yml 用）
#   scripts/version.sh set 0.2.9      改根 Cargo.toml、crates/lancat-slint、crates/lancat-ffi（之后刷新 Cargo.lock）
# 安卓 versionName 直接读根 Cargo.toml，不用单独改。
set -euo pipefail
cd "$(dirname "$0")/.."
manifests=(crates/lancat-slint/Cargo.toml crates/lancat-ffi/Cargo.toml)
root() { awk '/^\[workspace\.package\]/{f=1;next} /^\[/{f=0} f&&/^version *=/{gsub(/.*= *"|".*/,"");print;exit}' Cargo.toml; }
pkg() { awk '/^\[package\]/{f=1;next} /^\[/{f=0} f&&/^version *=/{gsub(/.*= *"|".*/,"");print;exit}' "$1"; }
case "${1:-get}" in
  get) root ;;
  check)
    want="${2#v}"; ok=1; v="$(root)"
    [[ "$v" == "$want" ]] || { echo "根 Cargo.toml = $v，标签 = $want" >&2; ok=0; }
    for m in "${manifests[@]}"; do p="$(pkg "$m")"; [[ "$p" == "$v" ]] || { echo "$m = $p，应为 $v" >&2; ok=0; }; done
    [[ $ok == 1 ]] && echo "版本一致：$v" || exit 1 ;;
  set)
    new="${2:?用法: version.sh set X.Y.Z}"
    sed -i "/^\[workspace\.package\]/,/^\[/ s/^version *= *\"[^\"]*\"/version = \"$new\"/" Cargo.toml
    for m in "${manifests[@]}"; do sed -i "0,/^version *= *\"[^\"]*\"/ s//version = \"$new\"/" "$m"; done
    echo "已改为 $new；记得刷新各工作区的 Cargo.lock" ;;
  *) echo "用法: version.sh get|check vX.Y.Z|set X.Y.Z" >&2; exit 2 ;;
esac
