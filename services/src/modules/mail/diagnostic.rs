use std::{net::IpAddr, sync::LazyLock};

use regex::Regex;

const MAX_DETAILS: usize = 3;

pub(super) fn mbsync_progress(line: &str) -> Option<String> {
    static COUNTS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
            r"^(?:(?:far|near|master|slave) side: [0-9]+ messages, [0-9]+ recent|",
            r"Channels: [0-9]+ +Boxes: [0-9]+",
            r"(?: +(?:Far|Near|Master|Slave): \+[0-9]+ \*[0-9]+ (?:#[0-9]+ )?-[0-9]+){2})$",
        ))
        .expect("mbsync 计数格式有效")
    });
    if COUNTS.is_match(line) {
        return Some(line.into());
    }
    // 不把配置路径、存储名称或邮箱名称从详细输出复制到进度。
    let status = match line {
        "Synchronizing..." => "正在同步邮件。",
        _ if line.starts_with("Reading configuration file ") => "正在读取同步配置。",
        _ if line.starts_with("Channel ") => "正在处理同步通道。",
        _ if line.starts_with("Opening ") && line.contains(" store ") => "正在打开邮件存储。",
        _ if line.starts_with("Opening ") && line.contains(" box ") => "正在打开邮箱。",
        _ if line.starts_with("Loading ") && line.ends_with(" box...") => "正在读取邮箱中的邮件。",
        _ if line.starts_with("Connecting to ") => "正在连接 IMAP 服务器。",
        _ => return mbsync_detail(line),
    };
    Some(status.into())
}

pub(super) fn with_detail(mut message: String, command: &str, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let mut details = Vec::new();
    for line in stderr.lines().map(str::trim) {
        let detail = match command {
            "mbsync" => mbsync_detail(line),
            _ => mu_detail(line),
        };
        if let Some(detail) = detail
            && !details.contains(&detail)
        {
            details.push(detail);
            if details.len() == MAX_DETAILS {
                break;
            }
        }
    }
    if details.is_empty()
        && command == "mbsync"
        && let Some(host) = stderr.lines().find_map(|line| {
            server_name(line.trim().strip_prefix("No working address found for ")?)
        })
    {
        details.push(format!("无法连接 IMAP 服务器 {host} 的任何地址。"));
    }
    if !details.is_empty() {
        message.push(' ');
        message.push_str(&details.join(" "));
    } else if !stderr.trim().is_empty() {
        message.push_str("未能从命令输出识别原因。");
    }
    message
}

fn mbsync_detail(line: &str) -> Option<String> {
    // 只提取已知字段。IMAP 响应和 PassCmd 的输出可能含有凭据。
    let lower = line.to_ascii_lowercase();
    if let Some(rest) = line.strip_prefix("Cannot connect to ") {
        let (target, _) = rest.rsplit_once(": ").unwrap_or((rest, ""));
        let target = server_endpoint(target).unwrap_or_else(|| "IMAP 服务器".into());
        let reason = if lower.contains("timed out") || lower.contains("timeout") {
            "超时"
        } else if lower.contains("connection refused") {
            "被拒绝"
        } else if lower.contains("unreachable") || lower.contains("no route to host") {
            "失败，网络不可达"
        } else {
            "失败"
        };
        return Some(format!("连接 {target} {reason}。"));
    }
    if let Some(rest) = line.strip_prefix("Error: Cannot resolve server '") {
        let host = rest
            .split_once('\'')
            .and_then(|(host, _)| server_name(host));
        let target = host.unwrap_or("IMAP 服务器");
        return Some(format!("DNS 无法解析 {target}。"));
    }
    if let Some(rest) = line.strip_prefix("SSL error connecting ") {
        let target = rest
            .rsplit_once(": ")
            .and_then(|(target, _)| server_endpoint(target));
        return Some(format!(
            "与 {} 的 TLS 握手失败。",
            target.as_deref().unwrap_or("IMAP 服务器")
        ));
    }
    if lower.contains("certificate")
        && (lower.starts_with("error") || lower.contains("verification failed"))
    {
        return Some("IMAP 服务器证书验证失败。".into());
    }
    if (lower.starts_with("skipping account ") && lower.ends_with(", no password"))
        || lower.contains("password command")
        || lower.contains("passcmd failed")
        || lower.starts_with("looking up keychain failed:")
    {
        return Some("无法取得 IMAP 登录凭据。".into());
    }
    if lower.starts_with("imap command 'login ")
        || lower.starts_with("imap command 'authenticate ")
        || lower.contains("[authenticationfailed]")
        || lower.contains("acceptable authentication mechanism")
        || lower.contains("sasl authentication")
    {
        return Some("IMAP 登录认证失败。".into());
    }
    if lower.starts_with("maildir error:") {
        let detail = if lower.contains("permission denied") {
            "本机 Maildir 访问权限不足。"
        } else if lower.contains("no space left on device") {
            "本机 Maildir 所在磁盘空间不足。"
        } else {
            "本机 Maildir 访问失败。请检查邮箱目录和权限。"
        };
        return Some(detail.into());
    }
    if line.starts_with("Cannot open config file ") {
        return Some("无法打开 mbsync 配置文件。".into());
    }
    if line == "No channel or group named 'loki-auto' defined."
        || line.starts_with("No channels defined.")
    {
        return Some("mbsync 配置缺少 loki-auto 同步组或可用 Channel。".into());
    }
    if let Some((location, _)) = line.split_once(": ")
        && location.starts_with('/')
        && let Some((_, number)) = location.rsplit_once(':')
        && let Ok(number) = number.parse::<u32>()
    {
        return Some(format!("mbsync 配置第 {number} 行有误。"));
    }
    if lower.starts_with("imap command 'select ") || lower.starts_with("imap command 'examine ") {
        return Some("IMAP 服务器无法打开配置中的邮箱文件夹。".into());
    }
    if lower.starts_with("imap error:") || lower.starts_with("imap command ") {
        return Some("IMAP 服务器请求失败。".into());
    }
    if lower.starts_with("socket error:") {
        return Some("IMAP 连接建立后读写失败。".into());
    }
    None
}

fn server_endpoint(target: &str) -> Option<String> {
    let Some((host, endpoint)) = target.split_once(" (") else {
        return Some(server_name(target)?.to_owned());
    };
    let host = server_name(host)?;
    let endpoint = endpoint.strip_suffix(')')?;
    let port = endpoint.rsplit_once(':').and_then(|(ip, port)| {
        ip.trim_matches(['[', ']']).parse::<IpAddr>().ok()?;
        port.parse::<u16>().ok()
    });
    match port {
        Some(port) if host.contains(':') => Some(format!("[{host}]:{port}")),
        Some(port) => Some(format!("{host}:{port}")),
        None => Some(host.to_owned()),
    }
}

fn server_name(host: &str) -> Option<&str> {
    let valid = host.parse::<IpAddr>().is_ok()
        || (!host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')));
    valid.then_some(host)
}

fn mu_detail(line: &str) -> Option<String> {
    let line = line.to_ascii_lowercase();
    let reason = match line.as_str() {
        _ if line.contains("database locked") => "mu 索引数据库被其他进程占用。",
        _ if line.contains("schema") => "mu 索引数据库的版本不兼容。",
        _ if line.contains("permission denied") => "mu 访问 Maildir 或索引数据库的权限不足。",
        _ if line.contains("no space left on device") => "mu 无法写入索引，磁盘空间不足。",
        _ if line.contains("mu init") => "mu 找不到可用的已初始化索引数据库。",
        _ if line.contains("failed to open database") => "mu 无法打开索引数据库。",
        _ if line.contains("maildir") && line.contains("not readable") => "mu 无法读取 Maildir。",
        _ => return None,
    };
    Some(reason.into())
}

#[cfg(test)]
mod tests {
    use super::with_detail;

    #[test]
    fn reports_imap_timeout_with_host_and_port_once() {
        let message = with_detail(
            "mbsync 执行失败（退出码 1）。".into(),
            "mbsync",
            b"Cannot connect to imap.gmail.com (192.0.2.1:993): Operation timed out\n\
              Cannot connect to imap.gmail.com (192.0.2.2:993): Operation timed out\n\
              No working address found for imap.gmail.com\n",
        );
        assert_eq!(
            message,
            "mbsync 执行失败（退出码 1）。 连接 imap.gmail.com:993 超时。"
        );
    }

    #[test]
    fn identifies_failures_without_echoing_credentials() {
        let message = with_detail(
            String::new(),
            "mbsync",
            b"Error: Cannot resolve server 'imap.example.org': Name or service not known\n\
              IMAP command 'LOGIN user TOP-SECRET' returned an error: NO [AUTHENTICATIONFAILED]\n\
              SSL error connecting imap2.example.org (192.0.2.2:993): certificate verify failed",
        );
        assert!(message.contains("DNS 无法解析 imap.example.org"));
        assert!(message.contains("IMAP 登录认证失败"));
        assert!(message.contains("imap2.example.org:993 的 TLS 握手失败"));
        assert!(!message.contains("TOP-SECRET"));
    }

    #[test]
    fn includes_mbsync_configuration_line_and_mu_database_error() {
        assert!(
            with_detail(
                String::new(),
                "mbsync",
                b"/tmp/mbsyncrc:12: keyword 'BadOption' is not recognized"
            )
            .contains("mbsync 配置第 12 行有误")
        );
        assert!(
            with_detail(
                String::new(),
                "mu find",
                b"error: failed to open database\nhint: Try (re)creating using \x60mu init'"
            )
            .contains("mu 找不到可用的已初始化索引数据库")
        );
    }
}
