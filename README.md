# IPv6 Proxy Pool（Rust 重构版）

[![Rust](https://img.shields.io/badge/Language-Rust-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

这是一个基于 **Rust（Tokio 异步运行时）重构** 的高性能 IPv6 SOCKS5 代理池工具。

工具可以自动扫描服务器上的 `/64` IPv6 子网，并利用该子网生成大量随机 IPv6 地址作为出口 IP，实现高并发与动态轮换的代理服务。

---

## ⚡️ 为什么选择 Rust 版本？

相比原来的 Go 版本，Rust 重构带来了显著提升：

* **极致性能**：Tokio 异步 I/O，每核可处理数万级并发连接。
* **内存安全**：无 GC，内存占用小且稳定，没有长时间运行后的抖动。
* **系统自动优化**：内置 `rlimit` 提升 File Descriptors，解决 “Too many open files”。
* **更强鲁棒性**：独立连接处理，单连接错误不会影响主程序。

---

## 🛠 功能特性

* **SOCKS5 协议支持**（无认证模式）
* **动态 IPv6 地址池**：自动识别 `/64` 子网并生成随机 IP
* **智能管理机制**：交互式清理旧 IP、生成新 IP
* **高性能零拷贝转发**：依赖系统底层调用实现极高效率

---

## 📋 环境要求

* **操作系统**：Linux（依赖 `ip` 命令）
* **权限要求**：需 `root` 权限（管理网卡 + 监听端口）
* **构建工具**：Rust（Cargo）

---

## 🚀 快速开始

### 1. 编译项目

```bash
# 克隆项目
git clone https://github.com/mkr-0920/ipv6_proxy_pool_rust.git
cd ipv6_proxy_pool

# 编译 Release 版本
cargo build --release
```

编译完成后，二进制文件位于：

```
target/release/ipv6_proxy_pool
```

---

### 2. 创建配置文件

在运行目录创建 `config.ini`：

```ini
[default]
# 拥有 /64 IPv6 地址块的物理网卡名称 (使用 ip addr 查看)
Networkname = eth0

# SOCKS5 代理监听端口
port = 1080
```

---

### 3. 运行程序

程序需操作网卡 IP 与监听端口，因此必须使用 `sudo`：

```bash
sudo ./target/release/ipv6_proxy_pool
```

---

### 4. 启动时的交互操作

启动后程序会引导你完成：

* **清理旧 IP**：自动识别历史生成但不属于 `/64` 基准的 IP
* **生成新 IP**：例如输入 `500` 或 `1000` 生成随机 IPv6 地址

---

## 🧪 测试代理是否工作

可以使用 CURL 测试 SOCKS5 出口 IP：

```bash
curl --socks5 127.0.0.1:1080 https://api64.ipify.org
```

多次执行输出的 IPv6 会不断变化，即表示代理池正常工作。

---
