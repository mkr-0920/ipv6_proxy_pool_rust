use anyhow::{anyhow, Result};
use ini::Ini;
use rand::Rng;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use get_if_addrs::get_if_addrs;

// 全局配置结构
struct AppConfig {
    network_name: String,
    port: String,
}

// 共享状态
struct AppState {
    ipv6_addresses: Vec<String>,
    counter: AtomicUsize,
}

#[tokio::main]
async fn main() -> Result<()> {
    // === [修复 1] 启动时自动解除系统文件句柄限制 ===
    match rlimit::increase_nofile_limit(u64::MAX) {
        Ok(limit) => println!("系统文件句柄限制已提升至: {}", limit),
        Err(e) => eprintln!("警告: 无法提升文件句柄限制 (可能需要 root 权限): {}", e),
    }

    // 1. 系统检查
    if cfg!(target_os = "windows") {
        println!("检测到系统: Windows");
    } else {
        println!("检测到系统: Linux");
    }

    // 2. 加载配置
    let conf = Ini::load_from_file("config.ini").map_err(|e| anyhow!("无法加载 config.ini: {}", e))?;
    let section = conf.section(Some("default"))
        .or_else(|| conf.section(None::<String>))
        .ok_or(anyhow!("config.ini 格式错误: 未找到 [default] 段"))?;
    
    let config = AppConfig {
        network_name: section.get("Networkname").unwrap_or("eth0").to_string(),
        port: section.get("port").unwrap_or("1080").to_string(),
    };
    println!("使用的网卡名称: {}", config.network_name);

    // 3. 扫描 IP
    let (keep_ips, all_public_ips) = scan_interface_ips(&config.network_name)?;
    
    if keep_ips.is_empty() {
        println!("警告: 未检测到任何 /64 前缀的 IPv6 地址。");
    } else {
        println!("发现基准 (/64) IPv6 地址: {:?}", keep_ips);
    }

    // 4. 交互：删除旧 IP
    if prompt_for_yes_no("是否删除除 /64 地址以外的 IPv6 地址(!!!)") {
        println!("开始删除地址...");
        let mut del_count = 0;
        for ip in &all_public_ips {
            if !keep_ips.contains(ip) {
                if let Err(e) = manage_ip("del", &config.network_name, ip) {
                    println!("删除 {} 失败: {}", ip, e);
                } else {
                    del_count += 1;
                }
            }
        }
        println!("删除完成，共清理 {} 个地址", del_count);
    }

    // 5. 交互：添加新 IP
    if prompt_for_yes_no("是否要添加新的 IPv6 地址") {
        let base_ip = if !keep_ips.is_empty() {
            keep_ips[0].clone()
        } else if !all_public_ips.is_empty() {
            println!("警告: 使用第一个现有公网 IP 作为基准...");
            all_public_ips[0].clone()
        } else {
            return Err(anyhow!("无法添加 IP: 找不到任何可用的 IPv6 基准地址"));
        };

        print!("请输入添加数量: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let count: usize = input.trim().parse().unwrap_or(0);

        println!("开始生成并添加 {} 个地址...", count);
        let new_ips = generate_random_ipv6_batch(&base_ip, count);
        
        let mut add_count = 0;
        for ip in new_ips {
            if let Err(e) = manage_ip("add", &config.network_name, &ip) {
                println!("添加 {} 失败: {}", ip, e);
            } else {
                add_count += 1;
            }
            thread::sleep(Duration::from_millis(2)); // 稍微缩短延时以加快速度
        }
        println!("添加完成，成功添加 {} 个地址", add_count);
    }

    // 6. 重新获取 IP
    let (_, final_ips) = scan_interface_ips(&config.network_name)?;
    if final_ips.is_empty() {
        return Err(anyhow!("错误: 当前没有任何可用的公网 IPv6 地址"));
    }
    
    println!("当前共有 {} 个可用的 IPv6 地址用于代理池。", final_ips.len());

    let state = Arc::new(AppState {
        ipv6_addresses: final_ips,
        counter: AtomicUsize::new(0),
    });

    // 7. 启动 SOCKS5
    let addr = format!("0.0.0.0:{}", config.port);
    let listener = TcpListener::bind(&addr).await?;
    println!("SOCKS5 代理正在监听 {}...", addr);

    // === [修复 2] 更健壮的连接接受循环 ===
    loop {
        // 使用 match 处理 accept 的结果，而不是用 ? 直接抛出
        match listener.accept().await {
            Ok((client_socket, _)) => {
                let state_clone = state.clone();
                tokio::spawn(async move {
                    if let Err(_e) = handle_client(client_socket, state_clone).await {
                        // 生产环境可以注释掉这行，减少日志刷屏
                        // eprintln!("连接处理错误: {}", _e);
                    }
                });
            }
            Err(e) => {
                // 如果发生错误（如文件句柄耗尽），打印错误并休眠一小会儿，防止 CPU 空转
                eprintln!("Accept 错误: {} (程序继续运行)", e);
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

// === 核心逻辑函数 ===

fn scan_interface_ips(iface_name: &str) -> Result<(Vec<String>, Vec<String>)> {
    let ifaces = get_if_addrs()?;
    let mut keep_ips = Vec::new();
    let mut all_public_ips = Vec::new();

    let mask_64: Ipv6Addr = "ffff:ffff:ffff:ffff::".parse().unwrap();

    for iface in ifaces {
        if iface.name == iface_name {
            if let get_if_addrs::IfAddr::V6(ifv6) = iface.addr {
                let addr_v6 = ifv6.ip;
                // 排除回环和非公网 (2000::/3)
                if !addr_v6.is_loopback() && addr_v6.segments()[0] >= 0x2000 {
                    let ip_str = addr_v6.to_string();
                    all_public_ips.push(ip_str.clone());

                    if ifv6.netmask == mask_64 {
                        keep_ips.push(ip_str);
                    }
                }
            }
        }
    }
    Ok((keep_ips, all_public_ips))
}

fn prompt_for_yes_no(prompt: &str) -> bool {
    let mut input = String::new();
    loop {
        print!("{} (y/n): ", prompt);
        let _ = io::stdout().flush();
        input.clear();
        if io::stdin().read_line(&mut input).is_err() { return false; }
        let trimmed = input.trim().to_lowercase();
        if trimmed == "y" || trimmed == "yes" { return true; }
        else if trimmed == "n" || trimmed == "no" { return false; }
        println!("请输入 'y' 或 'n'");
    }
}

fn manage_ip(action: &str, iface: &str, ip: &str) -> Result<()> {
    let status = Command::new("ip")
        .args(&["addr", action, &format!("{}/128", ip), "dev", iface])
        .output()?;
    if status.status.success() { Ok(()) } else {
        let err_msg = String::from_utf8_lossy(&status.stderr);
        Err(anyhow!("命令失败: {}", err_msg.trim()))
    }
}

fn generate_random_ipv6_batch(base_ip: &str, count: usize) -> Vec<String> {
    let ip: Ipv6Addr = base_ip.parse().expect("Invalid Base IP");
    let segments = ip.segments();
    let prefix = [segments[0], segments[1], segments[2], segments[3]];
    
    let mut rng = rand::thread_rng();
    let mut results = Vec::new();

    for _ in 0..count {
        let suffix: [u16; 4] = [rng.r#gen(), rng.r#gen(), rng.r#gen(), rng.r#gen()];
        let new_ip = Ipv6Addr::new(
            prefix[0], prefix[1], prefix[2], prefix[3],
            suffix[0], suffix[1], suffix[2], suffix[3]
        );
        results.push(new_ip.to_string());
    }
    results
}

async fn handle_client(mut client: TcpStream, state: Arc<AppState>) -> Result<()> {
    let mut buf = [0u8; 256];
    if client.read(&mut buf).await? < 2 || buf[0] != 0x05 { return Err(anyhow!("Err")); }
    client.write_all(&[0x05, 0x00]).await?;

    let n = client.read(&mut buf).await?;
    if n < 4 || buf[1] != 0x01 { return Err(anyhow!("Err")); }

    let dest = match buf[3] {
        0x01 => format!("{}:{}", std::net::Ipv4Addr::new(buf[4], buf[5], buf[6], buf[7]), u16::from_be_bytes([buf[8], buf[9]])),
        0x03 => {
            let len = buf[4] as usize;
            format!("{}:{}", String::from_utf8_lossy(&buf[5..5+len]), u16::from_be_bytes([buf[5+len], buf[5+len+1]]))
        },
        0x04 => {
            let mut oct = [0u8; 16]; oct.copy_from_slice(&buf[4..20]);
            format!("[{}]:{}", std::net::Ipv6Addr::from(oct), u16::from_be_bytes([buf[20], buf[21]]))
        },
        _ => return Err(anyhow!("Err")),
    };

    let idx = state.counter.fetch_add(1, Ordering::Relaxed) % state.ipv6_addresses.len();
    let bind_ip: Ipv6Addr = state.ipv6_addresses[idx].parse()?;

    let socket = TcpSocket::new_v6()?;
    socket.bind(SocketAddr::new(IpAddr::V6(bind_ip), 0))?;
    
    // 连接建立超时设置（防止卡住占用资源）
    let connect_future = async {
        let dest_addrs: Vec<SocketAddr> = tokio::net::lookup_host(&dest).await?.collect();
        if dest_addrs.is_empty() { return Err(anyhow!("DNS Err")); }
        socket.connect(dest_addrs[0]).await.map_err(|e| anyhow::Error::new(e))
    };

    let server = tokio::time::timeout(Duration::from_secs(10), connect_future).await
        .map_err(|_| anyhow!("Connect Timeout"))??;

    client.write_all(&[0x05, 0x00, 0x00, 0x01, 0,0,0,0, 0,0]).await?;

    let (mut cr, mut cw) = client.split();
    let (mut sr, mut sw) = server.into_split();
    let _ = tokio::join!(tokio::io::copy(&mut cr, &mut sw), tokio::io::copy(&mut sr, &mut cw));
    Ok(())
}